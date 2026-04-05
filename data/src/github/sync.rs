// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Bidirectional sync between Workgraph and GitHub Issues.

use octocrab::Octocrab;
use octocrab::models::issues::Issue;
use sqlx::SqlitePool;

use crate::sparks::error::SparksError;
use crate::sparks::types::*;
use crate::sparks::{comment_repo, spark_repo, stamp_repo};

pub struct GitHubSync {
    client: Octocrab,
    owner: String,
    repo: String,
}

pub struct SyncReport {
    pub pushed: usize,
    pub pulled: usize,
    pub errors: Vec<String>,
}

impl GitHubSync {
    pub fn new(token: &str, owner: &str, repo: &str) -> Result<Self, SparksError> {
        let client = Octocrab::builder()
            .personal_token(token.to_string())
            .build()
            .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

        Ok(Self {
            client,
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// Push a spark to GitHub. Creates a new issue or updates an existing one.
    /// Returns the GitHub issue number.
    pub async fn push_spark(&self, pool: &SqlitePool, spark: &Spark) -> Result<u64, SparksError> {
        let stamps = stamp_repo::list_for_spark(pool, &spark.id).await?;
        let mut labels: Vec<String> = stamps.iter().map(|s| s.name.clone()).collect();

        // Add priority label
        labels.push(format!("P{}", spark.priority));

        let issues = self.client.issues(&self.owner, &self.repo);

        if let Some(issue_num) = spark.github_issue_number {
            // Update existing issue
            let mut update = issues.update(issue_num as u64);
            update = update.title(&spark.title).body(&spark.description);

            if spark.status == "closed" {
                update = update.state(octocrab::models::IssueState::Closed);
            } else {
                update = update.state(octocrab::models::IssueState::Open);
            }

            update
                .send()
                .await
                .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

            // Sync labels
            issues
                .replace_all_labels(issue_num as u64, &labels)
                .await
                .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

            Ok(issue_num as u64)
        } else {
            // Create new issue
            let mut create = issues.create(&spark.title).body(&spark.description);

            if !labels.is_empty() {
                create = create.labels(labels);
            }

            if let Some(ref assignee) = spark.assignee {
                create = create.assignees(vec![assignee.clone()]);
            }

            let issue = create
                .send()
                .await
                .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

            let issue_number = issue.number as i32;

            // Store the issue number back on the spark
            sqlx::query("UPDATE sparks SET github_issue_number = ?, github_repo = ? WHERE id = ?")
                .bind(issue_number)
                .bind(format!("{}/{}", self.owner, self.repo))
                .bind(&spark.id)
                .execute(pool)
                .await?;

            Ok(issue.number)
        }
    }

    /// Pull a GitHub issue and upsert it as a spark.
    pub async fn pull_issue(
        &self,
        pool: &SqlitePool,
        issue_number: u64,
        workshop_id: &str,
    ) -> Result<Spark, SparksError> {
        let issue: Issue = self
            .client
            .issues(&self.owner, &self.repo)
            .get(issue_number)
            .await
            .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

        let github_repo = format!("{}/{}", self.owner, self.repo);

        // Check if we already have this issue
        let existing = sqlx::query_as::<_, Spark>(
            "SELECT * FROM sparks WHERE github_issue_number = ? AND github_repo = ?",
        )
        .bind(issue_number as i32)
        .bind(&github_repo)
        .fetch_optional(pool)
        .await?;

        if let Some(existing) = existing {
            // Update existing spark
            let status = if issue.state == octocrab::models::IssueState::Closed {
                Some(SparkStatus::Closed)
            } else {
                Some(SparkStatus::Open)
            };

            spark_repo::update(
                pool,
                &existing.id,
                UpdateSpark {
                    title: Some(issue.title),
                    description: issue.body.map(|b| b.to_string()),
                    status,
                    ..Default::default()
                },
                "github-sync",
            )
            .await
        } else {
            // Create new spark from issue
            let new = NewSpark {
                title: issue.title,
                description: issue.body.map(|b| b.to_string()).unwrap_or_default(),
                spark_type: SparkType::Task,
                priority: 2,
                workshop_id: workshop_id.to_string(),
                assignee: issue.assignee.map(|a| a.login),
                owner: None,
                parent_id: None,
                due_at: None,
                estimated_minutes: None,
                metadata: None,
            };

            let spark = spark_repo::create(pool, new).await?;

            // Link to GitHub issue
            sqlx::query("UPDATE sparks SET github_issue_number = ?, github_repo = ? WHERE id = ?")
                .bind(issue_number as i32)
                .bind(&github_repo)
                .bind(&spark.id)
                .execute(pool)
                .await?;

            // Sync labels as stamps
            let labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
            stamp_repo::set(pool, &spark.id, &labels).await?;

            spark_repo::get(pool, &spark.id).await
        }
    }

    /// Close a GitHub issue when a spark is closed.
    pub async fn close_issue(&self, issue_number: u64, reason: &str) -> Result<(), SparksError> {
        let issues = self.client.issues(&self.owner, &self.repo);

        // Add closing comment
        issues
            .create_comment(issue_number, format!("Closed: {reason}"))
            .await
            .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

        // Close the issue
        issues
            .update(issue_number)
            .state(octocrab::models::IssueState::Closed)
            .send()
            .await
            .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

        Ok(())
    }

    /// Push all sparks that have changed since last sync.
    pub async fn push_all(
        &self,
        pool: &SqlitePool,
        workshop_id: &str,
    ) -> Result<SyncReport, SparksError> {
        let sparks = spark_repo::list(
            pool,
            SparkFilter {
                workshop_id: Some(workshop_id.to_string()),
                ..Default::default()
            },
        )
        .await?;

        let mut report = SyncReport {
            pushed: 0,
            pulled: 0,
            errors: Vec::new(),
        };

        for spark in &sparks {
            match self.push_spark(pool, spark).await {
                Ok(_) => report.pushed += 1,
                Err(e) => report.errors.push(format!("{}: {e}", spark.id)),
            }
        }

        Ok(report)
    }

    /// Pull all open GitHub issues into sparks.
    pub async fn pull_all(
        &self,
        pool: &SqlitePool,
        workshop_id: &str,
    ) -> Result<SyncReport, SparksError> {
        let page = self
            .client
            .issues(&self.owner, &self.repo)
            .list()
            .state(octocrab::params::State::Open)
            .per_page(100)
            .send()
            .await
            .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

        let mut report = SyncReport {
            pushed: 0,
            pulled: 0,
            errors: Vec::new(),
        };

        for issue in page.items {
            // Skip pull requests (they show up in the issues endpoint)
            if issue.pull_request.is_some() {
                continue;
            }
            match self.pull_issue(pool, issue.number, workshop_id).await {
                Ok(_) => report.pulled += 1,
                Err(e) => report.errors.push(format!("#{}: {e}", issue.number)),
            }
        }

        Ok(report)
    }

    /// Sync comments between a spark and its GitHub issue.
    pub async fn sync_comments(&self, pool: &SqlitePool, spark: &Spark) -> Result<(), SparksError> {
        let Some(issue_num) = spark.github_issue_number else {
            return Ok(());
        };

        let gh_comments = self
            .client
            .issues(&self.owner, &self.repo)
            .list_comments(issue_num as u64)
            .send()
            .await
            .map_err(|e| SparksError::GitHubSync(e.to_string()))?;

        let local_comments = comment_repo::list_for_spark(pool, &spark.id).await?;
        let local_bodies: std::collections::HashSet<&str> =
            local_comments.iter().map(|c| c.body.as_str()).collect();

        // Pull new GitHub comments
        for gc in &gh_comments.items {
            let body = gc.body.as_deref().unwrap_or("");
            if !local_bodies.contains(body) {
                let author = gc.user.login.clone();

                comment_repo::create(
                    pool,
                    NewComment {
                        spark_id: spark.id.clone(),
                        author,
                        body: body.to_string(),
                    },
                )
                .await?;
            }
        }

        Ok(())
    }
}
