// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Loomantix

//! Font family name interning.
//!
//! `iced::font::Family::Name` requires a `&'static str`, but font family
//! names are loaded dynamically from user config and the settings panel.
//! Naively calling `Box::leak` on every font change permanently leaks a
//! heap allocation per call, so memory grows monotonically as the user
//! cycles font sizes (Cmd+Scroll), opens panes, etc.
//!
//! This module interns family names so that each unique name leaks at
//! most once. Repeated lookups for the same name return the same
//! `&'static str` with zero new allocations. Spark sp-27a217db.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn cache() -> &'static Mutex<HashMap<String, &'static str>> {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return a `&'static str` for the given font family name, allocating
/// (and leaking) at most one buffer per unique name across the lifetime
/// of the process. Subsequent calls with the same name reuse the cached
/// pointer.
pub fn intern(name: &str) -> &'static str {
    let mut guard = cache().lock().expect("font intern cache poisoned");
    if let Some(&existing) = guard.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    guard.insert(name.to_owned(), leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_intern_returns_same_pointer() {
        let a = intern("Fira Code");
        let b = intern("Fira Code");
        assert_eq!(a.as_ptr(), b.as_ptr());
        assert_eq!(a, "Fira Code");
    }

    #[test]
    fn distinct_names_get_distinct_storage() {
        let a = intern("JetBrains Mono");
        let b = intern("Menlo");
        assert_ne!(a.as_ptr(), b.as_ptr());
        assert_eq!(a, "JetBrains Mono");
        assert_eq!(b, "Menlo");
    }

    #[test]
    fn many_calls_with_same_name_do_not_grow_cache() {
        // Use a name unlikely to collide with other tests in this module.
        let unique = "font-intern-stress-test-family";
        let first = intern(unique);
        let before = cache().lock().unwrap().len();
        for _ in 0..1000 {
            let p = intern(unique);
            assert_eq!(p.as_ptr(), first.as_ptr());
        }
        let after = cache().lock().unwrap().len();
        assert_eq!(before, after);
    }
}
