// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Headless smoke test for the global keyboard subscription's dispatch
//! routing.
//!
//! Spark `ryve-5b9c5d93` — Performance regression harness.
//!
//! Why this exists: a previous iteration of `App::subscription` swallowed
//! every unmatched key event by mapping it to `Message::SparksPoll`,
//! which fans out into a full workgraph reload + N agent-session queries.
//! A fast typist could re-trigger that load several times per second.
//!
//! This test drives a synthetic key burst through the *exact* dispatch
//! routine the binary uses (`perf_core::classify_key_event`) and asserts
//! the SparksPoll dispatch count stays at zero. If anyone re-introduces
//! the antipattern this test fails on every CI run.

use perf_core::{KeyDispatch, KeyKind, KeyModifiers, classify_key_event};

#[test]
fn synthetic_key_burst_does_not_dispatch_sparks_poll() {
    let mut sparks_poll_count = 0usize;
    let mut total = 0usize;

    // 1000 unmatched character keystrokes — what an aggressive typist or a
    // held-down repeat would produce in a couple of seconds.
    for c in ('a'..='z').cycle().take(1000) {
        total += 1;
        let dispatch = classify_key_event(KeyKind::Character(c), KeyModifiers::default());
        if dispatch == KeyDispatch::SparksPoll {
            sparks_poll_count += 1;
        }
    }

    // Mix in modifier-change events, escape, and the "other" bucket — none
    // of these should ever be SparksPoll either.
    for shift in [false, true, false, true].iter().copied() {
        total += 1;
        let dispatch =
            classify_key_event(KeyKind::ModifiersChanged { shift }, KeyModifiers::default());
        if dispatch == KeyDispatch::SparksPoll {
            sparks_poll_count += 1;
        }
    }
    for _ in 0..50 {
        total += 1;
        let dispatch = classify_key_event(KeyKind::Escape, KeyModifiers::default());
        if dispatch == KeyDispatch::SparksPoll {
            sparks_poll_count += 1;
        }
        total += 1;
        let dispatch = classify_key_event(KeyKind::Other, KeyModifiers::default());
        if dispatch == KeyDispatch::SparksPoll {
            sparks_poll_count += 1;
        }
    }

    assert!(total >= 1000, "burst should be substantial");
    assert_eq!(
        sparks_poll_count, 0,
        "key dispatch must never resolve to SparksPoll — that was the perf bug \
         this regression test exists to catch"
    );
}

#[test]
fn known_hotkeys_still_route_correctly() {
    // Sanity: the dispatcher hasn't been gutted into an all-Noop function.
    let cmd = KeyModifiers { command: true };
    assert_eq!(
        classify_key_event(KeyKind::Character('h'), cmd),
        KeyDispatch::NewDefaultHand
    );
    assert_eq!(
        classify_key_event(KeyKind::Character('o'), cmd),
        KeyDispatch::NewWorkshopDialog
    );
    assert_eq!(
        classify_key_event(KeyKind::Escape, KeyModifiers::default()),
        KeyDispatch::HotkeyEscape
    );
}
