// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JOURNAL: one line per thing the harness did or declined to do, appended to
//! `<state>/journal.log`. Never the terminal: the vendor's screen owns it while it runs.
//!
//! `<state>` is [`STATE_ENV`] when set (absolute), else
//! `~/Library/Application Support/grok-alab` on macOS, else `$XDG_STATE_HOME/grok-alab` or
//! `~/.local/state/grok-alab`. A journal that cannot be written is skipped: it never holds
//! the session up.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env::Env;

/// The variable that overrides the state directory (an absolute path).
pub const STATE_ENV: &str = "GROK_ALAB_STATE_DIR";

/// The journal's file name inside the state directory.
pub const FILE: &str = "journal.log";

/// Past this size the journal is rotated to `journal.log.1`.
const ROTATE_BYTES: u64 = 1024 * 1024;

/// Where the journal goes, if anywhere.
#[derive(Debug, Clone)]
pub struct Journal {
    path: Option<PathBuf>,
    pid: u32,
}

impl Journal {
    /// The journal under the state directory `env` resolves.
    #[must_use]
    pub fn open(env: &Env) -> Journal {
        Journal {
            path: state_dir(env).map(|d| d.join(FILE)),
            pid: std::process::id(),
        }
    }

    /// A journal that writes nothing.
    #[must_use]
    pub fn none() -> Journal {
        Journal {
            path: None,
            pid: std::process::id(),
        }
    }

    /// The journal file, when there is one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Append `<unix seconds> pid=<pid> <WORD> <text>`.
    pub fn line(&self, word: &str, text: &str) {
        let Some(path) = &self.path else { return };
        let _ = self.append(path, word, text);
    }

    fn append(&self, path: &Path, word: &str, text: &str) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        if fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > ROTATE_BYTES) {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let _ = fs::rename(path, PathBuf::from(old));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("journal is not a regular file"));
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let text: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        // One `write` per line: on a file opened for appending, each write lands whole at the
        // end, so the harnesses of other sessions sharing this journal never interleave inside
        // a line. A `writeln!` on the file would write each formatted piece on its own.
        file.write_all(format!("{secs} pid={} {word} {text}\n", self.pid).as_bytes())
    }
}

/// The state directory `env` resolves, or `None` with no usable home.
#[must_use]
pub fn state_dir(env: &Env) -> Option<PathBuf> {
    if let Some(v) = env.get(STATE_ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        return p.is_absolute().then_some(p);
    }
    let home = env
        .get("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())?;
    if cfg!(target_os = "macos") {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("grok-alab"),
        );
    }
    let state = env
        .get("XDG_STATE_HOME")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .filter(|x| x.is_absolute())
        .unwrap_or_else(|| home.join(".local").join("state"));
    Some(state.join("grok-alab"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_dir_override_must_be_absolute() {
        let env = Env::from_pairs([(STATE_ENV, "/s/grok")]);
        assert_eq!(state_dir(&env), Some(PathBuf::from("/s/grok")));
        let env = Env::from_pairs([(STATE_ENV, "rel")]);
        assert_eq!(state_dir(&env), None);
    }

    /// Every session's harness appends to the one journal. Measured 2026-10-25 under aterm:
    /// four `grok` launches at once left lines like `1792969520 pid=1792969520955471792969520
    /// pid= pid=…`, because each line went out as one `write` per formatted piece. Sixteen
    /// writers here, each with its own journal and file, must leave only whole lines, each
    /// exactly as one writer wrote it.
    #[test]
    fn harnesses_sharing_the_journal_never_interleave_inside_a_line() {
        let dir = crate::scratch::Scratch::new("journal");
        let path = dir.join(FILE);
        const WRITERS: usize = 16;
        const LINES: usize = 200;
        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let path = path.clone();
                s.spawn(move || {
                    let journal = Journal {
                        path: Some(path),
                        pid: 1000 + u32::try_from(w).unwrap(),
                    };
                    let text = format!("writer {w} {}", "x".repeat(200));
                    for _ in 0..LINES {
                        journal.line("NOTE", &text);
                    }
                });
            }
        });
        let body = fs::read_to_string(&path).unwrap();
        let mut seen = [0usize; WRITERS];
        for line in body.lines() {
            let mut words = line.splitn(4, ' ');
            let (secs, pid, word, text) = (
                words.next().unwrap_or(""),
                words.next().unwrap_or(""),
                words.next().unwrap_or(""),
                words.next().unwrap_or(""),
            );
            let w = pid
                .strip_prefix("pid=")
                .and_then(|p| p.parse::<usize>().ok())
                .and_then(|p| p.checked_sub(1000))
                .filter(|w| *w < WRITERS);
            let whole = secs.parse::<u64>().is_ok()
                && word == "NOTE"
                && w.is_some_and(|w| text == format!("writer {w} {}", "x".repeat(200)));
            assert!(whole, "a line two writers interleaved: {line:?}");
            seen[w.unwrap()] += 1;
        }
        assert_eq!(seen, [LINES; WRITERS], "every line of every writer, once");
    }

    #[test]
    fn with_no_override_the_state_dir_is_under_home() {
        let env = Env::from_pairs([("HOME", "/h")]);
        let want = if cfg!(target_os = "macos") {
            "/h/Library/Application Support/grok-alab"
        } else {
            "/h/.local/state/grok-alab"
        };
        assert_eq!(state_dir(&env), Some(PathBuf::from(want)));
        assert_eq!(state_dir(&Env::default()), None);
    }
}
