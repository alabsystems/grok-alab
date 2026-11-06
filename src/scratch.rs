// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unit tests' scratch directories: each removes itself, and everything in it, when it is
//! dropped, a failing test's too. Before it, every run of the credential tests left its four
//! scratch homes behind in the system temp dir (90 on one machine by 2026-10-25).

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A fresh, empty directory `grok-alab-<name>-<pid>-<n>` under the system temp dir, removed
/// when dropped, on a panic as on a pass.
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// Makes the directory, emptying a stale one a dead process with the same pid left.
    pub(crate) fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "grok-alab-{name}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_is_gone_after_its_test_passes_or_panics() {
        let passed = {
            let s = Scratch::new("scratch-pass");
            std::fs::create_dir_all(s.join(".grok")).unwrap();
            std::fs::write(s.join(".grok").join("auth.json"), b"").unwrap();
            assert!(s.join(".grok").join("auth.json").is_file(), "CONTROL");
            s.to_path_buf()
        };
        assert!(!passed.exists(), "{}", passed.display());

        let mut panicked = None;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let s = Scratch::new("scratch-panic");
            std::fs::create_dir_all(s.join(".grok")).unwrap();
            std::fs::write(s.join(".grok").join("auth.json"), b"").unwrap();
            panicked = Some(s.to_path_buf());
            panic!("a failing test");
        }));
        assert!(caught.is_err(), "CONTROL: the closure panicked");
        let panicked = panicked.unwrap();
        assert!(!panicked.exists(), "{}", panicked.display());

        let (a, b) = (Scratch::new("scratch-two"), Scratch::new("scratch-two"));
        assert_ne!(
            &*a, &*b,
            "two scratches of one name in one process are distinct"
        );
    }
}
