// SPDX-License-Identifier: AGPL-3.0-or-later

//! Inbound IRC command parser and dispatcher.
//!
//! Ryve's coordination contract treats IRC as a canonical surface: a user or
//! agent typing `/ryve transition asgn-abc in_progress expected=assigned` in
//! a channel must produce the same lifecycle event it would have produced
//! via the programmatic API, through the same transition validator and the
//! same event outbox. Free-text chatter in those channels must NEVER mutate
//! state — only `/ryve`-prefixed messages do.
//!
//! # Surface
//!
//! The module is split into three layers so each is independently testable:
//!
//! 1. [`parse`] — pure string → [`Command`]. Rejects anything that is not a
//!    `/ryve`-prefixed line ([`ParseError::NotACommand`]); rejects malformed
//!    commands with a typed [`ParseError`].
//! 2. [`CommandExecutor`] — async trait that the relay wires to the real
//!    DB-backed transition validator / outbox writer / read-only status
//!    query. Mock implementations drive the unit tests here.
//! 3. [`dispatch`] — thin glue: parse, execute, and return a
//!    [`DispatchOutcome`] telling the caller whether to emit a reply (and
//!    whether that reply is a PRIVMSG or a NOTICE).
//!
//! # Invariants
//!
//! - Non-`/ryve` messages produce [`DispatchOutcome::Ignored`] and never
//!   touch the executor — guaranteeing free-text never mutates state.
//! - Every mutating command (`transition`, `review`, `blocker`) reaches the
//!   relay's [`CommandExecutor`], which MUST route through the same
//!   validator/outbox path the programmatic API uses. The executor is the
//!   single seam where authorization and transition-legality are enforced.
//! - [`Command::Status`] is read-only: its executor method queries state
//!   and returns a [`StatusSnapshot`] that the dispatcher renders as a
//!   PRIVMSG reply. It does NOT enqueue an outbox event.
//! - On executor failure the dispatcher produces a NOTICE carrying the
//!   typed error — never a silent drop.

use std::future::Future;
use std::pin::Pin;

/// Known assignment phase names. Mirrors the `AssignmentPhase` enum in the
/// `data` crate; kept as a local allow-list so the parser stays free of a
/// `data`-crate dependency and so mistyped phase names produce a specific
/// parse error instead of a generic validation failure at the executor.
const KNOWN_PHASES: &[&str] = &[
    "assigned",
    "in_progress",
    "awaiting_review",
    "approved",
    "rejected",
    "in_repair",
    "ready_for_merge",
    "merged",
];

/// Leading token every inbound command must start with.
const COMMAND_PREFIX: &str = "/ryve";

/// A parsed, validated IRC command. Variants mirror the acceptance criteria
/// one-for-one so each sub-parser has a stable target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/ryve transition <asg_id> <phase> expected=<phase>` — advance an
    /// assignment's phase. `expected_phase` is the caller's view of the
    /// current phase and is forwarded to the validator so out-of-order
    /// replays are rejected.
    Transition {
        asg_id: String,
        target_phase: String,
        expected_phase: String,
    },
    /// `/ryve review approve <asg_id> [summary]` or
    /// `/ryve review reject <asg_id> [summary]` — reviewer verdict.
    Review {
        asg_id: String,
        decision: ReviewDecision,
        summary: Option<String>,
    },
    /// `/ryve blocker <asg_id> "reason"` — raise a blocker on an assignment.
    /// `reason` is a single quoted argument; unquoted trailing text is
    /// rejected to keep the surface unambiguous.
    Blocker { asg_id: String, reason: String },
    /// `/ryve status <asg_id>` — read-only snapshot. Never goes through the
    /// outbox; the dispatcher resolves it to a PRIVMSG reply directly.
    Status { asg_id: String },
}

/// Reviewer verdict for `/ryve review`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Approve,
    Reject,
}

impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Reject => "reject",
        }
    }
}

/// Typed parse failure. Every variant produces a human-readable message via
/// `Display` that is safe to send back over IRC as a NOTICE.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Message is not a `/ryve` command — the relay should ignore it
    /// entirely and not emit any reply.
    #[error("not a /ryve command")]
    NotACommand,

    /// `/ryve` with no subcommand.
    #[error("missing subcommand: expected one of transition, review, blocker, status")]
    MissingSubcommand,

    /// Subcommand name not recognised.
    #[error("unknown subcommand {0:?}: expected one of transition, review, blocker, status")]
    UnknownSubcommand(String),

    /// A required positional argument was not supplied.
    #[error("missing argument: {0}")]
    MissingArg(&'static str),

    /// An argument failed a format check (quoting, keyword shape, etc.).
    #[error("invalid argument {arg}: {detail}")]
    InvalidArg { arg: &'static str, detail: String },

    /// A phase name that is not in [`KNOWN_PHASES`].
    #[error("unknown phase {phase:?}: expected one of {}", KNOWN_PHASES.join(", "))]
    UnknownPhase { phase: String },

    /// A quoted argument was opened but not closed, or contained invalid
    /// escape sequences.
    #[error("malformed quoted string: {0}")]
    MalformedQuotedString(String),

    /// Trailing tokens remained after the parser consumed every documented
    /// argument for the subcommand. Rejected to keep the surface sharp.
    #[error("unexpected trailing input: {0:?}")]
    UnexpectedTrailingInput(String),
}

/// Typed executor failure. The dispatcher renders each variant as an IRC
/// NOTICE to the sender. Unlike [`ParseError`] these mirror runtime
/// conditions the relay discovers when it walks the DB.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    /// Assignment id did not resolve to a row.
    #[error("unknown assignment {0}")]
    UnknownAssignment(String),

    /// Transition validator rejected the requested phase change.
    #[error("bad transition: {0}")]
    BadTransition(String),

    /// The sender's identity or role is not authorized for this command.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// Anything else — DB error, outbox write failure, etc. Kept as a
    /// string so this crate stays free of a direct `data`-crate dep.
    #[error("internal error: {0}")]
    Internal(String),
}

/// A read-only snapshot of an assignment, returned by
/// [`CommandExecutor::status`] and rendered by the dispatcher as a PRIVMSG
/// reply. The fields correspond to the acceptance criterion "replies with
/// assignment phase + owner + last event".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    pub asg_id: String,
    pub phase: String,
    pub owner: String,
    /// Short description of the most recent event (e.g. `"assignment_phase:
    /// in_progress"`), already formatted for display. `None` means the
    /// assignment has no events yet.
    pub last_event: Option<String>,
}

/// Convenience alias for the boxed future returned by [`CommandExecutor`]
/// methods. Keeps the trait dyn-compatible without pulling in `async_trait`
/// or `futures::BoxFuture`.
pub type ExecFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ExecError>> + Send + 'a>>;

/// The seam between the IRC parser and Ryve's real state-mutation path.
///
/// The relay implements this against the programmatic API: `transition`
/// calls `data::sparks::transition::transition_assignment_phase`, `review`
/// calls the same validator with the Approved/Rejected target and
/// ReviewerHand role, `blocker` writes a blocker event to the outbox, and
/// `status` reads straight from `assignments` / `events`. Tests implement
/// a mock and drive every code path through it.
pub trait CommandExecutor: Send + Sync {
    fn transition<'a>(
        &'a self,
        sender: &'a str,
        asg_id: &'a str,
        target_phase: &'a str,
        expected_phase: &'a str,
    ) -> ExecFuture<'a, ()>;

    fn review<'a>(
        &'a self,
        sender: &'a str,
        asg_id: &'a str,
        decision: ReviewDecision,
        summary: Option<&'a str>,
    ) -> ExecFuture<'a, ()>;

    fn blocker<'a>(
        &'a self,
        sender: &'a str,
        asg_id: &'a str,
        reason: &'a str,
    ) -> ExecFuture<'a, ()>;

    fn status<'a>(&'a self, asg_id: &'a str) -> ExecFuture<'a, StatusSnapshot>;
}

/// Reply kind for [`IrcReply`]. PRIVMSG is used for read-only query
/// responses; NOTICE is used for errors so clients can style them
/// distinctly from normal chatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrcReplyKind {
    Privmsg,
    Notice,
}

/// Concrete message the relay should send back. `target` is the channel the
/// original command came from; `body` is the already-formatted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrcReply {
    pub kind: IrcReplyKind,
    pub target: String,
    pub body: String,
}

/// What the relay should do with an inbound PRIVMSG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Not a `/ryve` command — the relay should ignore it completely.
    Ignored,
    /// Command ran. If `reply` is `Some` the relay should emit it now
    /// (status query or an immediate ack). If `None` the command was a
    /// mutation whose confirmation will flow through the outbox relay on
    /// the next drain — no direct reply needed.
    Handled { reply: Option<IrcReply> },
    /// Command rejected. The relay should emit `reply` as a NOTICE.
    Rejected { reply: IrcReply },
}

/// Parse a raw PRIVMSG body into a [`Command`].
///
/// The empty string and any line that does not begin with the `/ryve`
/// token produce [`ParseError::NotACommand`] so the dispatcher can
/// distinguish "ignore silently" from "reply with a typed error".
pub fn parse(text: &str) -> Result<Command, ParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ParseError::NotACommand);
    }

    let mut rest = match trimmed.strip_prefix(COMMAND_PREFIX) {
        // `/ryvefoo` is NOT a `/ryve` command — require whitespace or EOL.
        Some(r) if r.is_empty() || r.starts_with(char::is_whitespace) => r,
        _ => return Err(ParseError::NotACommand),
    };
    rest = rest.trim_start();

    let (sub, args) = split_first_token(rest);
    if sub.is_empty() {
        return Err(ParseError::MissingSubcommand);
    }

    match sub {
        "transition" => parse_transition(args),
        "review" => parse_review(args),
        "blocker" => parse_blocker(args),
        "status" => parse_status(args),
        other => Err(ParseError::UnknownSubcommand(other.to_string())),
    }
}

/// Parse, dispatch, and render the reply for an inbound PRIVMSG.
///
/// `sender` is the IRC nick of the author; `channel` is where the command
/// came from and where any reply is sent. This function is the only path
/// the relay needs to call — it handles parse errors, executor errors, and
/// happy-path rendering.
pub async fn dispatch(
    executor: &dyn CommandExecutor,
    sender: &str,
    channel: &str,
    text: &str,
) -> DispatchOutcome {
    let command = match parse(text) {
        Ok(c) => c,
        Err(ParseError::NotACommand) => return DispatchOutcome::Ignored,
        Err(err) => {
            return DispatchOutcome::Rejected {
                reply: notice(channel, &err.to_string()),
            };
        }
    };

    execute(executor, sender, channel, command).await
}

async fn execute(
    executor: &dyn CommandExecutor,
    sender: &str,
    channel: &str,
    command: Command,
) -> DispatchOutcome {
    match command {
        Command::Transition {
            asg_id,
            target_phase,
            expected_phase,
        } => match executor
            .transition(sender, &asg_id, &target_phase, &expected_phase)
            .await
        {
            Ok(()) => DispatchOutcome::Handled { reply: None },
            Err(e) => DispatchOutcome::Rejected {
                reply: notice(channel, &format!("/ryve transition: {e}")),
            },
        },
        Command::Review {
            asg_id,
            decision,
            summary,
        } => match executor
            .review(sender, &asg_id, decision, summary.as_deref())
            .await
        {
            Ok(()) => DispatchOutcome::Handled { reply: None },
            Err(e) => DispatchOutcome::Rejected {
                reply: notice(channel, &format!("/ryve review: {e}")),
            },
        },
        Command::Blocker { asg_id, reason } => {
            match executor.blocker(sender, &asg_id, &reason).await {
                Ok(()) => DispatchOutcome::Handled { reply: None },
                Err(e) => DispatchOutcome::Rejected {
                    reply: notice(channel, &format!("/ryve blocker: {e}")),
                },
            }
        }
        Command::Status { asg_id } => match executor.status(&asg_id).await {
            Ok(snapshot) => DispatchOutcome::Handled {
                reply: Some(privmsg(channel, &render_status(&snapshot))),
            },
            Err(e) => DispatchOutcome::Rejected {
                reply: notice(channel, &format!("/ryve status: {e}")),
            },
        },
    }
}

fn notice(target: &str, body: &str) -> IrcReply {
    IrcReply {
        kind: IrcReplyKind::Notice,
        target: target.to_string(),
        body: body.to_string(),
    }
}

fn privmsg(target: &str, body: &str) -> IrcReply {
    IrcReply {
        kind: IrcReplyKind::Privmsg,
        target: target.to_string(),
        body: body.to_string(),
    }
}

fn render_status(s: &StatusSnapshot) -> String {
    let last = s.last_event.as_deref().unwrap_or("(none)");
    format!(
        "assignment {}: phase={} owner={} last_event={}",
        s.asg_id, s.phase, s.owner, last
    )
}

// ── Sub-parsers ────────────────────────────────────────

fn parse_transition(args: &str) -> Result<Command, ParseError> {
    let args = args.trim_start();
    let (asg_id, rest) = split_first_token(args);
    if asg_id.is_empty() {
        return Err(ParseError::MissingArg("<asg_id>"));
    }

    let rest = rest.trim_start();
    let (target_phase, rest) = split_first_token(rest);
    if target_phase.is_empty() {
        return Err(ParseError::MissingArg("<phase>"));
    }
    validate_phase(target_phase)?;

    let rest = rest.trim_start();
    let (expected_token, rest) = split_first_token(rest);
    if expected_token.is_empty() {
        return Err(ParseError::MissingArg("expected=<phase>"));
    }
    let expected_phase = parse_expected_kv(expected_token)?;
    validate_phase(&expected_phase)?;

    reject_trailing(rest)?;

    Ok(Command::Transition {
        asg_id: asg_id.to_string(),
        target_phase: target_phase.to_string(),
        expected_phase,
    })
}

fn parse_review(args: &str) -> Result<Command, ParseError> {
    let args = args.trim_start();
    let (verdict, rest) = split_first_token(args);
    if verdict.is_empty() {
        return Err(ParseError::MissingArg("approve|reject"));
    }
    let decision = match verdict {
        "approve" => ReviewDecision::Approve,
        "reject" => ReviewDecision::Reject,
        other => {
            return Err(ParseError::InvalidArg {
                arg: "verdict",
                detail: format!("expected approve|reject, got {other:?}"),
            });
        }
    };

    let rest = rest.trim_start();
    let (asg_id, rest) = split_first_token(rest);
    if asg_id.is_empty() {
        return Err(ParseError::MissingArg("<asg_id>"));
    }

    // Anything remaining is the optional summary — free-form, trimmed.
    let summary = {
        let s = rest.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    };

    Ok(Command::Review {
        asg_id: asg_id.to_string(),
        decision,
        summary,
    })
}

fn parse_blocker(args: &str) -> Result<Command, ParseError> {
    let args = args.trim_start();
    let (asg_id, rest) = split_first_token(args);
    if asg_id.is_empty() {
        return Err(ParseError::MissingArg("<asg_id>"));
    }

    let rest = rest.trim_start();
    if rest.is_empty() {
        return Err(ParseError::MissingArg("\"reason\""));
    }

    let (reason, remainder) = parse_quoted_string(rest)?;
    reject_trailing(remainder.trim_start())?;

    Ok(Command::Blocker {
        asg_id: asg_id.to_string(),
        reason,
    })
}

fn parse_status(args: &str) -> Result<Command, ParseError> {
    let args = args.trim_start();
    let (asg_id, rest) = split_first_token(args);
    if asg_id.is_empty() {
        return Err(ParseError::MissingArg("<asg_id>"));
    }
    reject_trailing(rest)?;

    Ok(Command::Status {
        asg_id: asg_id.to_string(),
    })
}

// ── Lexing helpers ─────────────────────────────────────

/// Split `input` at the first whitespace boundary, returning
/// `(first_token, remainder)`. Both sides may be empty.
fn split_first_token(input: &str) -> (&str, &str) {
    match input.find(char::is_whitespace) {
        Some(i) => (&input[..i], &input[i..]),
        None => (input, ""),
    }
}

fn validate_phase(phase: &str) -> Result<(), ParseError> {
    if KNOWN_PHASES.contains(&phase) {
        Ok(())
    } else {
        Err(ParseError::UnknownPhase {
            phase: phase.to_string(),
        })
    }
}

/// Parse the `expected=<phase>` keyword argument, returning the bare phase
/// name. Leading / trailing whitespace is not expected here — the caller
/// already split on whitespace.
fn parse_expected_kv(token: &str) -> Result<String, ParseError> {
    match token.strip_prefix("expected=") {
        Some(v) if !v.is_empty() => Ok(v.to_string()),
        Some(_) => Err(ParseError::InvalidArg {
            arg: "expected",
            detail: "expected=<phase> was empty".into(),
        }),
        None => Err(ParseError::InvalidArg {
            arg: "expected",
            detail: format!("expected=<phase> required, got {token:?}"),
        }),
    }
}

/// Reject trailing tokens after a subcommand's documented args.
fn reject_trailing(rest: &str) -> Result<(), ParseError> {
    let trailing = rest.trim();
    if trailing.is_empty() {
        Ok(())
    } else {
        Err(ParseError::UnexpectedTrailingInput(trailing.to_string()))
    }
}

/// Consume a double-quoted string starting at `input[0] == '"'`. Supports
/// the minimal escape set `\"` and `\\`; any other backslash escape is a
/// [`ParseError::MalformedQuotedString`]. Returns the decoded inner string
/// plus the remainder of `input` after the closing quote.
fn parse_quoted_string(input: &str) -> Result<(String, &str), ParseError> {
    let mut chars = input.char_indices();
    match chars.next() {
        Some((_, '"')) => {}
        _ => {
            return Err(ParseError::MalformedQuotedString(
                "reason must be wrapped in double quotes".into(),
            ));
        }
    }

    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                // Closing quote — return decoded body + remainder past the quote.
                let end = i + c.len_utf8();
                return Ok((out, &input[end..]));
            }
            '\\' => match chars.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, other)) => {
                    return Err(ParseError::MalformedQuotedString(format!(
                        "unsupported escape sequence \\{other}"
                    )));
                }
                None => {
                    return Err(ParseError::MalformedQuotedString(
                        "dangling backslash at end of input".into(),
                    ));
                }
            },
            other => out.push(other),
        }
    }

    Err(ParseError::MalformedQuotedString(
        "missing closing double quote".into(),
    ))
}

// ── Tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    // ── Parser tests (pure, no executor) ───────────────

    #[test]
    fn empty_input_is_not_a_command() {
        assert_eq!(parse(""), Err(ParseError::NotACommand));
        assert_eq!(parse("   "), Err(ParseError::NotACommand));
    }

    #[test]
    fn free_text_is_not_a_command() {
        assert_eq!(
            parse("hello there, how's the build?"),
            Err(ParseError::NotACommand)
        );
        // Mentions of /ryve inside a message do not count.
        assert_eq!(
            parse("talking about /ryve transition here"),
            Err(ParseError::NotACommand)
        );
    }

    #[test]
    fn ryve_prefix_must_be_followed_by_whitespace_or_end() {
        // `/ryvelike` shares the prefix but is a different token — must not
        // be treated as a `/ryve` command.
        assert_eq!(
            parse("/ryvelike status asgn-abc"),
            Err(ParseError::NotACommand)
        );
    }

    #[test]
    fn bare_ryve_with_no_subcommand_is_rejected() {
        assert_eq!(parse("/ryve"), Err(ParseError::MissingSubcommand));
        assert_eq!(parse("/ryve   "), Err(ParseError::MissingSubcommand));
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        match parse("/ryve wobble asgn-abc") {
            Err(ParseError::UnknownSubcommand(s)) => assert_eq!(s, "wobble"),
            other => panic!("expected UnknownSubcommand, got {other:?}"),
        }
    }

    // ── /ryve transition ───────────────────────────────

    #[test]
    fn transition_happy_path() {
        let cmd = parse("/ryve transition asgn-abc in_progress expected=assigned").unwrap();
        assert_eq!(
            cmd,
            Command::Transition {
                asg_id: "asgn-abc".into(),
                target_phase: "in_progress".into(),
                expected_phase: "assigned".into(),
            }
        );
    }

    #[test]
    fn transition_tolerates_extra_whitespace() {
        let cmd =
            parse("   /ryve   transition   asgn-abc   awaiting_review   expected=in_progress  ")
                .unwrap();
        assert!(matches!(cmd, Command::Transition { .. }));
    }

    #[test]
    fn transition_missing_asg_id() {
        assert_eq!(
            parse("/ryve transition"),
            Err(ParseError::MissingArg("<asg_id>"))
        );
    }

    #[test]
    fn transition_missing_target_phase() {
        assert_eq!(
            parse("/ryve transition asgn-abc"),
            Err(ParseError::MissingArg("<phase>"))
        );
    }

    #[test]
    fn transition_missing_expected_kv() {
        assert_eq!(
            parse("/ryve transition asgn-abc in_progress"),
            Err(ParseError::MissingArg("expected=<phase>"))
        );
    }

    #[test]
    fn transition_unknown_target_phase() {
        match parse("/ryve transition asgn-abc wobbly expected=assigned") {
            Err(ParseError::UnknownPhase { phase }) => assert_eq!(phase, "wobbly"),
            other => panic!("expected UnknownPhase for target, got {other:?}"),
        }
    }

    #[test]
    fn transition_unknown_expected_phase() {
        match parse("/ryve transition asgn-abc in_progress expected=wobbly") {
            Err(ParseError::UnknownPhase { phase }) => assert_eq!(phase, "wobbly"),
            other => panic!("expected UnknownPhase for expected, got {other:?}"),
        }
    }

    #[test]
    fn transition_malformed_expected_kv() {
        match parse("/ryve transition asgn-abc in_progress foo=bar") {
            Err(ParseError::InvalidArg { arg, .. }) => assert_eq!(arg, "expected"),
            other => panic!("expected InvalidArg for expected kv, got {other:?}"),
        }
    }

    #[test]
    fn transition_rejects_trailing_tokens() {
        match parse("/ryve transition asgn-abc in_progress expected=assigned extra junk") {
            Err(ParseError::UnexpectedTrailingInput(s)) => assert_eq!(s, "extra junk"),
            other => panic!("expected UnexpectedTrailingInput, got {other:?}"),
        }
    }

    // ── /ryve review ───────────────────────────────────

    #[test]
    fn review_approve_without_summary() {
        let cmd = parse("/ryve review approve asgn-abc").unwrap();
        assert_eq!(
            cmd,
            Command::Review {
                asg_id: "asgn-abc".into(),
                decision: ReviewDecision::Approve,
                summary: None,
            }
        );
    }

    #[test]
    fn review_reject_with_summary() {
        let cmd = parse("/ryve review reject asgn-abc looks good but tests failing").unwrap();
        assert_eq!(
            cmd,
            Command::Review {
                asg_id: "asgn-abc".into(),
                decision: ReviewDecision::Reject,
                summary: Some("looks good but tests failing".into()),
            }
        );
    }

    #[test]
    fn review_missing_verdict() {
        assert_eq!(
            parse("/ryve review"),
            Err(ParseError::MissingArg("approve|reject"))
        );
    }

    #[test]
    fn review_missing_asg_id() {
        assert_eq!(
            parse("/ryve review approve"),
            Err(ParseError::MissingArg("<asg_id>"))
        );
    }

    #[test]
    fn review_invalid_verdict() {
        match parse("/ryve review maybe asgn-abc") {
            Err(ParseError::InvalidArg { arg, detail }) => {
                assert_eq!(arg, "verdict");
                assert!(detail.contains("maybe"));
            }
            other => panic!("expected InvalidArg(verdict), got {other:?}"),
        }
    }

    // ── /ryve blocker ──────────────────────────────────

    #[test]
    fn blocker_happy_path() {
        let cmd = parse("/ryve blocker asgn-abc \"CI is red on main\"").unwrap();
        assert_eq!(
            cmd,
            Command::Blocker {
                asg_id: "asgn-abc".into(),
                reason: "CI is red on main".into(),
            }
        );
    }

    #[test]
    fn blocker_escape_sequences() {
        let cmd = parse("/ryve blocker asgn-abc \"quote: \\\" and backslash: \\\\\"").unwrap();
        let Command::Blocker { reason, .. } = cmd else {
            panic!("expected Blocker");
        };
        assert_eq!(reason, "quote: \" and backslash: \\");
    }

    #[test]
    fn blocker_missing_asg_id() {
        assert_eq!(
            parse("/ryve blocker"),
            Err(ParseError::MissingArg("<asg_id>"))
        );
    }

    #[test]
    fn blocker_missing_quoted_reason() {
        assert_eq!(
            parse("/ryve blocker asgn-abc"),
            Err(ParseError::MissingArg("\"reason\""))
        );
    }

    #[test]
    fn blocker_unquoted_reason_is_rejected() {
        match parse("/ryve blocker asgn-abc ci is red") {
            Err(ParseError::MalformedQuotedString(_)) => {}
            other => panic!("expected MalformedQuotedString, got {other:?}"),
        }
    }

    #[test]
    fn blocker_unterminated_quote() {
        match parse("/ryve blocker asgn-abc \"still typing") {
            Err(ParseError::MalformedQuotedString(d)) => assert!(d.contains("closing")),
            other => panic!("expected MalformedQuotedString, got {other:?}"),
        }
    }

    #[test]
    fn blocker_bad_escape() {
        match parse("/ryve blocker asgn-abc \"has \\n newline\"") {
            Err(ParseError::MalformedQuotedString(d)) => assert!(d.contains("escape")),
            other => panic!("expected MalformedQuotedString for bad escape, got {other:?}"),
        }
    }

    #[test]
    fn blocker_rejects_trailing_input_after_quote() {
        match parse("/ryve blocker asgn-abc \"reason\" and more") {
            Err(ParseError::UnexpectedTrailingInput(s)) => assert_eq!(s, "and more"),
            other => panic!("expected UnexpectedTrailingInput, got {other:?}"),
        }
    }

    // ── /ryve status ───────────────────────────────────

    #[test]
    fn status_happy_path() {
        assert_eq!(
            parse("/ryve status asgn-abc").unwrap(),
            Command::Status {
                asg_id: "asgn-abc".into()
            }
        );
    }

    #[test]
    fn status_missing_asg_id() {
        assert_eq!(
            parse("/ryve status"),
            Err(ParseError::MissingArg("<asg_id>"))
        );
    }

    #[test]
    fn status_rejects_trailing_tokens() {
        match parse("/ryve status asgn-abc please") {
            Err(ParseError::UnexpectedTrailingInput(s)) => assert_eq!(s, "please"),
            other => panic!("expected UnexpectedTrailingInput, got {other:?}"),
        }
    }

    // ── Dispatcher tests with mock executor ────────────

    /// Mock executor that records every call and returns either a canned
    /// success or a canned error — whichever the individual test sets.
    #[derive(Default)]
    struct MockExecutor {
        calls: Mutex<Vec<String>>,
        transition_err: Mutex<Option<ExecError>>,
        review_err: Mutex<Option<ExecError>>,
        blocker_err: Mutex<Option<ExecError>>,
        status_result: Mutex<Option<Result<StatusSnapshot, ExecError>>>,
    }

    impl MockExecutor {
        fn record(&self, s: String) {
            self.calls.lock().unwrap().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandExecutor for MockExecutor {
        fn transition<'a>(
            &'a self,
            sender: &'a str,
            asg_id: &'a str,
            target: &'a str,
            expected: &'a str,
        ) -> ExecFuture<'a, ()> {
            Box::pin(async move {
                self.record(format!("transition({sender},{asg_id},{target},{expected})"));
                if let Some(err) = self.transition_err.lock().unwrap().take() {
                    Err(err)
                } else {
                    Ok(())
                }
            })
        }

        fn review<'a>(
            &'a self,
            sender: &'a str,
            asg_id: &'a str,
            decision: ReviewDecision,
            summary: Option<&'a str>,
        ) -> ExecFuture<'a, ()> {
            Box::pin(async move {
                self.record(format!(
                    "review({sender},{asg_id},{},{:?})",
                    decision.as_str(),
                    summary
                ));
                if let Some(err) = self.review_err.lock().unwrap().take() {
                    Err(err)
                } else {
                    Ok(())
                }
            })
        }

        fn blocker<'a>(
            &'a self,
            sender: &'a str,
            asg_id: &'a str,
            reason: &'a str,
        ) -> ExecFuture<'a, ()> {
            Box::pin(async move {
                self.record(format!("blocker({sender},{asg_id},{reason})"));
                if let Some(err) = self.blocker_err.lock().unwrap().take() {
                    Err(err)
                } else {
                    Ok(())
                }
            })
        }

        fn status<'a>(&'a self, asg_id: &'a str) -> ExecFuture<'a, StatusSnapshot> {
            Box::pin(async move {
                self.record(format!("status({asg_id})"));
                self.status_result
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or_else(|| {
                        Ok(StatusSnapshot {
                            asg_id: asg_id.to_string(),
                            phase: "in_progress".into(),
                            owner: "alice".into(),
                            last_event: Some("assignment_phase: in_progress".into()),
                        })
                    })
            })
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(f)
    }

    #[test]
    fn dispatch_ignores_non_command_messages() {
        let exec = MockExecutor::default();
        let out = block_on(dispatch(&exec, "alice", "#epic-1", "chit chat here"));
        assert_eq!(out, DispatchOutcome::Ignored);
        assert!(exec.calls().is_empty(), "executor must not be invoked");
    }

    #[test]
    fn dispatch_rejects_parse_error_with_notice() {
        let exec = MockExecutor::default();
        let out = block_on(dispatch(&exec, "alice", "#epic-1", "/ryve wobble"));
        match out {
            DispatchOutcome::Rejected { reply } => {
                assert_eq!(reply.kind, IrcReplyKind::Notice);
                assert_eq!(reply.target, "#epic-1");
                assert!(reply.body.contains("unknown subcommand"), "{}", reply.body);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
        assert!(exec.calls().is_empty());
    }

    #[test]
    fn dispatch_transition_queues_via_outbox() {
        let exec = Arc::new(MockExecutor::default());
        let out = block_on(dispatch(
            exec.as_ref(),
            "alice",
            "#epic-1",
            "/ryve transition asgn-abc in_progress expected=assigned",
        ));
        // Mutating commands have no immediate reply — the outbox relay
        // echoes the resulting event as the confirmation.
        assert_eq!(out, DispatchOutcome::Handled { reply: None });
        assert_eq!(
            exec.calls(),
            vec!["transition(alice,asgn-abc,in_progress,assigned)"]
        );
    }

    #[test]
    fn dispatch_transition_authorization_failure_replies_notice() {
        let exec = MockExecutor::default();
        *exec.transition_err.lock().unwrap() = Some(ExecError::Unauthorized(
            "hand alice cannot drive awaiting_review -> approved".into(),
        ));
        let out = block_on(dispatch(
            &exec,
            "alice",
            "#epic-1",
            "/ryve transition asgn-abc approved expected=awaiting_review",
        ));
        match out {
            DispatchOutcome::Rejected { reply } => {
                assert_eq!(reply.kind, IrcReplyKind::Notice);
                assert_eq!(reply.target, "#epic-1");
                assert!(reply.body.contains("unauthorized"));
                assert!(reply.body.contains("/ryve transition:"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_transition_unknown_assignment_replies_notice() {
        let exec = MockExecutor::default();
        *exec.transition_err.lock().unwrap() =
            Some(ExecError::UnknownAssignment("asgn-zzz".into()));
        let out = block_on(dispatch(
            &exec,
            "bob",
            "#epic-1",
            "/ryve transition asgn-zzz in_progress expected=assigned",
        ));
        match out {
            DispatchOutcome::Rejected { reply } => {
                assert!(reply.body.contains("unknown assignment asgn-zzz"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_review_approve_succeeds() {
        let exec = MockExecutor::default();
        let out = block_on(dispatch(
            &exec,
            "bob",
            "#epic-1",
            "/ryve review approve asgn-abc ship it",
        ));
        assert_eq!(out, DispatchOutcome::Handled { reply: None });
        assert_eq!(
            exec.calls(),
            vec!["review(bob,asgn-abc,approve,Some(\"ship it\"))"]
        );
    }

    #[test]
    fn dispatch_review_reject_error_flows_through_as_notice() {
        let exec = MockExecutor::default();
        *exec.review_err.lock().unwrap() = Some(ExecError::BadTransition(
            "awaiting_review -> rejected requires reviewer_hand role".into(),
        ));
        let out = block_on(dispatch(
            &exec,
            "alice",
            "#epic-1",
            "/ryve review reject asgn-abc tests failing",
        ));
        match out {
            DispatchOutcome::Rejected { reply } => {
                assert_eq!(reply.kind, IrcReplyKind::Notice);
                assert!(reply.body.contains("/ryve review:"));
                assert!(reply.body.contains("reviewer_hand"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_blocker_queues_via_outbox() {
        let exec = MockExecutor::default();
        let out = block_on(dispatch(
            &exec,
            "alice",
            "#epic-1",
            "/ryve blocker asgn-abc \"waiting on infra\"",
        ));
        assert_eq!(out, DispatchOutcome::Handled { reply: None });
        assert_eq!(
            exec.calls(),
            vec!["blocker(alice,asgn-abc,waiting on infra)"]
        );
    }

    #[test]
    fn dispatch_status_returns_privmsg_without_touching_outbox() {
        let exec = MockExecutor::default();
        *exec.status_result.lock().unwrap() = Some(Ok(StatusSnapshot {
            asg_id: "asgn-abc".into(),
            phase: "in_progress".into(),
            owner: "alice".into(),
            last_event: Some("assignment_phase: in_progress".into()),
        }));
        let out = block_on(dispatch(&exec, "bob", "#epic-1", "/ryve status asgn-abc"));
        match out {
            DispatchOutcome::Handled { reply: Some(reply) } => {
                assert_eq!(reply.kind, IrcReplyKind::Privmsg);
                assert_eq!(reply.target, "#epic-1");
                assert!(reply.body.contains("asgn-abc"));
                assert!(reply.body.contains("phase=in_progress"));
                assert!(reply.body.contains("owner=alice"));
                assert!(reply.body.contains("assignment_phase: in_progress"));
            }
            other => panic!("expected Handled with PRIVMSG, got {other:?}"),
        }
        assert_eq!(exec.calls(), vec!["status(asgn-abc)"]);
    }

    #[test]
    fn dispatch_status_with_no_events_shows_none() {
        let exec = MockExecutor::default();
        *exec.status_result.lock().unwrap() = Some(Ok(StatusSnapshot {
            asg_id: "asgn-new".into(),
            phase: "assigned".into(),
            owner: "carol".into(),
            last_event: None,
        }));
        let out = block_on(dispatch(&exec, "carol", "#epic-1", "/ryve status asgn-new"));
        let DispatchOutcome::Handled { reply: Some(reply) } = out else {
            panic!("expected Handled + reply");
        };
        assert!(reply.body.contains("last_event=(none)"));
    }

    #[test]
    fn dispatch_status_unknown_assignment_replies_notice() {
        let exec = MockExecutor::default();
        *exec.status_result.lock().unwrap() =
            Some(Err(ExecError::UnknownAssignment("asgn-zzz".into())));
        let out = block_on(dispatch(&exec, "carol", "#epic-1", "/ryve status asgn-zzz"));
        match out {
            DispatchOutcome::Rejected { reply } => {
                assert_eq!(reply.kind, IrcReplyKind::Notice);
                assert!(reply.body.contains("/ryve status:"));
                assert!(reply.body.contains("asgn-zzz"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    /// Invariant: free-text messages never reach the executor even if
    /// they mention assignment ids or phase names.
    #[test]
    fn free_text_never_invokes_executor() {
        let exec = MockExecutor::default();
        for text in [
            "hey @alice, did you transition asgn-abc to in_progress?",
            "let's do /ryve/transition on asgn-abc",
            "approve asgn-abc",
            "/ryvelike transition asgn-abc in_progress expected=assigned",
        ] {
            let out = block_on(dispatch(&exec, "alice", "#epic-1", text));
            assert_eq!(out, DispatchOutcome::Ignored, "text was: {text:?}");
        }
        assert!(exec.calls().is_empty());
    }
}
