// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Benchmark for the no-op path of `data::agent_context::sync`.
//!
//! "No-op path" = the second time `sync` runs against an already-populated
//! workshop. The first run writes WORKSHOP.md and injects markers into all
//! configured agent boot files; the second run rewrites the same content
//! to the same paths. The audit (spark ryve-27a217db) flagged this path as
//! a hot loop because the binary calls it on every workshop refresh, so
//! the regression harness keeps an eye on its cost.
//!
//! Spark ryve-5b9c5d93 — Performance regression harness.

use criterion::{Criterion, criterion_group, criterion_main};
use data::agent_context;
use data::ryve_dir::{RyveDir, WorkshopConfig};
use tempfile::TempDir;
use tokio::runtime::Runtime;

fn bench_agent_context_sync_noop(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");

    // One-time setup: create a fake workshop, run sync once so the second
    // call is exercising the steady-state "everything already up to date"
    // path that the binary hits on every workshop tick.
    let tmp = TempDir::new().expect("tempdir");
    let workshop_dir = tmp.path().to_path_buf();
    let ryve_dir = RyveDir::new(&workshop_dir);
    let config = WorkshopConfig::default();

    rt.block_on(async {
        ryve_dir.ensure_exists().await.expect("ensure_exists");
        agent_context::sync(&workshop_dir, &ryve_dir, &config)
            .await
            .expect("initial sync");
    });

    c.bench_function("agent_context_sync_noop", |b| {
        b.to_async(&rt).iter(|| async {
            agent_context::sync(&workshop_dir, &ryve_dir, &config)
                .await
                .expect("noop sync");
        });
    });
}

criterion_group!(benches, bench_agent_context_sync_noop);
criterion_main!(benches);
