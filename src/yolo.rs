// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE YOLO DEFAULT, CARRIED FROM ATERM'S LAUNCHER (aterm
//! `docs/DESIGN-harness-interface-2026-11-12.md` §2.1, "The launcher keeps its other duties").
//!
//! aterm's `grok` launcher puts Grok Build's permission flag in front of a launch that names no
//! posture of its own, expands the `--yolo` alias, and strips `--no-harness`, before it hands
//! the words to `grok-alab run --` (aterm `crates/atpkg/src/yolo.rs`, `decide`, `word_options`'s
//! `"grok"` arm and `strip_no_harness`; the shell form `sh_yolo_prelude_with_launcher` in
//! `crates/atpkg/src/platform/mod.rs`; the policy is the `grok` row's `yolo_flags`,
//! `yolo_markers`, `yolo_after` and `yolo_skip` in `crates/atpkg/src/vendor_direct/table.rs`).
//! Once this harness leads the word, the launcher runs `grok-alab run -- "$@"` with the words as
//! typed and deletes its block, so `run` makes the same decision here, byte for byte, and prints
//! the same line on stderr ([`echo_line`]).
//!
//! The decision reads the words ahead of a literal `--` ([`decide`]):
//! - a leading option this scan does not know ([`VALUES`], [`OPTIONAL`], [`SWITCHES`]), a value
//!   missing at the end, or a removal that would hand a bare word to an optional value runs as
//!   typed (grok's row has an `after` word, so this holds with a typed `--yolo` too);
//! - a typed `--yolo` becomes [`FLAGS`] (once each, right after `agent` when `agent` is the
//!   first word and the `--yolo` came before it), dropping grok's own spelling of that posture
//!   ([`OWN_YOLO`]) ahead of the first word ([`Launch::Flag`]);
//! - else a launch holding one of [`MARKERS`] runs as typed, and so does a first word of
//!   [`SKIP`] or the self-update verb ([`crate::selfupdate::VERBS`]);
//! - else [`FLAGS`] go in front, or right after an [`AFTER`] first word ([`Launch::Default`]).
//!
//! Tokens are compared as bytes, as the launcher's `case` patterns compare them, so an argument
//! that is not UTF-8 is carried, never refused.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;

/// The alias the launcher claims, exactly as typed.
pub const WORD: &str = "--yolo";

/// What a by-the-flag echo starts with (atpkg `yolo::ECHO_PREFIX`).
pub const ECHO_PREFIX: &str = "aterm: --yolo: running ";

/// What a by-default echo starts with (atpkg `yolo::DEFAULT_ECHO_PREFIX`).
pub const DEFAULT_ECHO_PREFIX: &str = "aterm: yolo (default): running ";

/// The program word the echo names (the row's `program`).
pub const PROGRAM: &str = "grok";

/// The words that leave the harness out of a launch (atpkg `yolo::strip_no_harness`).
pub const NO_HARNESS_WORDS: [&str; 2] = ["--no-harness", "--no-alab-harness"];

/// The vendor flags the posture expands to (the row's `yolo_flags`).
pub const FLAGS: &[&str] = &["--always-approve"];

/// The tokens that already decide a launch's posture (the row's `yolo_markers`). One ending in
/// `=` matches only its `--flag=value` form; a long one matches itself and its `=value` form.
pub const MARKERS: &[&str] = &[
    "--always-approve",
    "--dangerously-skip-permissions",
    "--permission-mode",
    "--permission-mode=",
    "--sandbox",
    "--sandbox=",
    "--allow",
    "--allow=",
    "--allowedTools",
    "--allowedTools=",
    "--deny",
    "--deny=",
    "--disallowedTools",
    "--disallowedTools=",
    "--disallowed-tools",
    "--disallowed-tools=",
];

/// First words the flags go behind (the row's `yolo_after`).
pub const AFTER: &[&str] = &["agent"];

/// First words that leave a launch without `--yolo` as typed (the row's `yolo_skip`).
pub const SKIP: &[&str] = &[
    "inspect",
    "doctor",
    "leader",
    "login",
    "logout",
    "mcp",
    "plugin",
    "memory",
    "models",
    "sessions",
    "usage",
    "setup",
    "share",
    "wrap",
    "export",
    "trace",
    "version",
    "v",
    "completions",
    "worktree",
    "du",
    "disk-usage",
    "workspace",
    "dashboard",
    "worker",
    "cursor-worker",
    "clone",
    "ssh",
    "host-server",
    "help",
];

/// Leading options that take the next token as their value (atpkg `word_options`'s `"grok"`
/// arm: grok's top-level `PagerArgs` plus 1.0.67's `--add-dir`).
pub const VALUES: &[&str] = &[
    "-m",
    "--model",
    "--cwd",
    "-p",
    "--single",
    "--print",
    "--prompt-file",
    "--prompt-json",
    "--load",
    "--leader-socket",
    "--debug-file",
    "--output-format",
    "--json-schema",
    "--reasoning-effort",
    "--effort",
    "--rules",
    "--append-system-prompt",
    "--system-prompt-override",
    "--system-prompt",
    "-s",
    "--session-id",
    "--worktree-ref",
    "--ref",
    "--agent",
    "--agents",
    "--tools",
    "--max-turns",
    "--add-dir",
];

/// Leading options whose value is optional: the next token is theirs unless it starts with `-`.
pub const OPTIONAL: &[&str] = &["-r", "--resume", "-w", "--worktree"];

/// Leading options that take no value.
pub const SWITCHES: &[&str] = &[
    "-c",
    "--continue",
    "--no-auto-update",
    "--debug",
    "--verbatim",
    "--include-partial-messages",
    "--fork-session",
    "--restore-code",
    "--no-plan",
    "--no-subagents",
    "--disable-web-search",
    "--no-alt-screen",
    "--minimal",
    "--fullscreen",
    "--oauth",
];

/// grok's own spelling of the posture a typed `--yolo` sets, an alias of `--always-approve` its
/// parser refuses twice: an explicit `--yolo` drops it ahead of the first word.
pub const OWN_YOLO: &[&str] = &["--dangerously-skip-permissions"];

/// Whether `token` is one of `list`, byte for byte.
pub(crate) fn listed(list: &[&str], token: &[u8]) -> bool {
    list.iter().any(|w| w.as_bytes() == token)
}

/// Whether `token` is decided by `marker` in every spelling clap gives the same flag: a marker
/// ending in `=` matches its `--flag=value` form only, a short flag also matches its attached
/// forms (`-mgrok-4`), and a long flag matches itself and its `--flag=value` form.
pub(crate) fn marker_matches(marker: &str, token: &[u8]) -> bool {
    let m = marker.as_bytes();
    if let Some(word) = m.strip_suffix(b"=") {
        return token.starts_with(word) && token.get(word.len()) == Some(&b'=');
    }
    if let [b'-', second] = m
        && *second != b'-'
    {
        return token.starts_with(m);
    }
    token == m || (token.starts_with(m) && token.get(m.len()) == Some(&b'='))
}

/// Why the word scan stopped short of the first word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// An option the scan does not know, or a value missing at the end.
    Unknown,
    /// A `--yolo` (or [`OWN_YOLO`]) right after a bare optional-value option and before a bare
    /// word: removing it would hand that word to the option as its value.
    Joins,
}

/// Whether `a` is a word an explicit `--yolo` removes.
fn removable(a: &[u8]) -> bool {
    a == WORD.as_bytes() || listed(OWN_YOLO, a)
}

/// The position of the first word (a subcommand or the prompt) in `head`, past the leading
/// options whose arity this scan knows (atpkg `yolo::first_word`).
fn first_word(head: &[OsString]) -> Result<Option<usize>, Stop> {
    let mut value = false;
    let mut maybe = false;
    let mut gap = false;
    for (i, arg) in head.iter().enumerate() {
        let a = arg.as_bytes();
        if value {
            value = false;
            continue;
        }
        if maybe {
            maybe = false;
            if !a.starts_with(b"-") {
                // The optional value.
                continue;
            }
            gap = removable(a);
        } else if gap && !removable(a) {
            if !a.starts_with(b"-") {
                return Err(Stop::Joins);
            }
            gap = false;
        }
        if removable(a) || listed(FLAGS, a) || listed(SWITCHES, a) {
            continue;
        } else if listed(VALUES, a) {
            value = true;
        } else if listed(OPTIONAL, a) {
            maybe = true;
        } else if VALUES
            .iter()
            .chain(OPTIONAL.iter())
            .any(|flag| marker_matches(flag, a))
        {
            // A long `=value` or an attached short value is already consumed.
            continue;
        } else if a.starts_with(b"-") {
            return Err(Stop::Unknown);
        } else {
            return Ok(Some(i));
        }
    }
    if value { Err(Stop::Unknown) } else { Ok(None) }
}

/// What a launch runs as ([`decide`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// The words as typed, nothing printed.
    AsTyped,
    /// `--yolo` was typed: these words, echoed with [`ECHO_PREFIX`].
    Flag(Vec<OsString>),
    /// Nothing decided the posture: these words, echoed with [`DEFAULT_ECHO_PREFIX`].
    Default(Vec<OsString>),
}

impl Launch {
    /// The prefix of the line this launch prints, when it prints one.
    #[must_use]
    pub const fn echo_prefix(&self) -> Option<&'static str> {
        match self {
            Self::AsTyped => None,
            Self::Flag(_) => Some(ECHO_PREFIX),
            Self::Default(_) => Some(DEFAULT_ECHO_PREFIX),
        }
    }

    /// The words this launch runs with: its own, or `typed` when it runs as typed.
    #[must_use]
    pub fn words(self, typed: Vec<OsString>) -> Vec<OsString> {
        match self {
            Self::AsTyped => typed,
            Self::Flag(words) | Self::Default(words) => words,
        }
    }
}

/// Push each of [`FLAGS`] not yet `present` onto `out` (atpkg `decide`'s `fresh`).
fn fresh(out: &mut Vec<OsString>, present: &mut Vec<&[u8]>) {
    for flag in FLAGS {
        if !present.contains(&flag.as_bytes()) {
            present.push(flag.as_bytes());
            out.push(OsString::from(flag));
        }
    }
}

/// Decide `grok args…` as aterm's launcher does (atpkg `yolo::decide` for the `grok` row), on
/// the words ahead of a literal `--`.
#[must_use]
pub fn decide(args: &[OsString]) -> Launch {
    let head_len = args.iter().take_while(|a| a.as_bytes() != b"--").count();
    let (head, tail) = args.split_at(head_len.min(args.len()));
    // grok's row has an `after` word, so an option the scan does not know runs as typed even
    // with a typed `--yolo` (atpkg: `Err(Stop::Unknown) if !explicit || !policy.after.is_empty()`).
    let Ok(word) = first_word(head) else {
        return Launch::AsTyped;
    };
    let explicit = head.iter().any(|a| a.as_bytes() == WORD.as_bytes());
    let leading = word.unwrap_or(head_len);
    let first = word.and_then(|i| head.get(i)).map(|w| w.as_bytes());
    let after = first.is_some_and(|w| listed(AFTER, w));
    if explicit {
        let mut present: Vec<&[u8]> = head.iter().map(|a| a.as_bytes()).collect();
        let mut out: Vec<OsString> = Vec::with_capacity(args.len() + FLAGS.len());
        let mut pending = false;
        for (i, arg) in head.iter().enumerate() {
            let a = arg.as_bytes();
            if a == WORD.as_bytes() {
                if after && word.is_some_and(|w| i < w) {
                    pending = true;
                } else {
                    fresh(&mut out, &mut present);
                }
            } else if i < leading && listed(OWN_YOLO, a) {
                // grok's own spelling of the posture the flags set.
            } else {
                out.push(arg.clone());
                if pending && word == Some(i) {
                    pending = false;
                    fresh(&mut out, &mut present);
                }
            }
        }
        out.extend(tail.iter().cloned());
        return Launch::Flag(out);
    }
    if head
        .iter()
        .any(|a| MARKERS.iter().any(|m| marker_matches(m, a.as_bytes())))
    {
        return Launch::AsTyped;
    }
    if let (Some(i), Some(w)) = (word, first) {
        // The self-update verb skips too: that shape is the intercept's, decided before this
        // block (`grok update --help` is grok's own help), and the default must not land on it.
        if listed(SKIP, w) || listed(crate::selfupdate::VERBS, w) {
            return Launch::AsTyped;
        }
        if after && let Some((upto, rest)) = args.split_at_checked(i.saturating_add(1)) {
            let mut out: Vec<OsString> = Vec::with_capacity(args.len() + FLAGS.len());
            out.extend(upto.iter().cloned());
            out.extend(FLAGS.iter().map(OsString::from));
            out.extend(rest.iter().cloned());
            return Launch::Default(out);
        }
    }
    let mut out: Vec<OsString> = FLAGS.iter().map(OsString::from).collect();
    out.extend(args.iter().cloned());
    Launch::Default(out)
}

/// The words with each of [`NO_HARNESS_WORDS`] ahead of a literal `--` removed, or `None` when
/// none was there (atpkg `yolo::strip_no_harness`, which also sets `ATERM_NO_HARNESS=1`).
#[must_use]
pub fn strip_no_harness(args: &[OsString]) -> Option<Vec<OsString>> {
    let end = args
        .iter()
        .position(|a| a.as_bytes() == b"--")
        .unwrap_or(args.len());
    let mut found = false;
    let mut out = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        if i < end && listed(&NO_HARNESS_WORDS, arg.as_bytes()) {
            found = true;
            continue;
        }
        out.push(arg.clone());
    }
    found.then_some(out)
}

/// Append `word` single-quoted as the launcher's `printf` loop spells it: every word quoted,
/// an embedded `'` written `'\''`.
fn quote_into(line: &mut Vec<u8>, word: &[u8]) {
    line.push(b'\'');
    for &b in word {
        if b == b'\'' {
            line.extend_from_slice(b"'\\''");
        } else {
            line.push(b);
        }
    }
    line.push(b'\'');
}

/// The one stderr line a launch the block changed prints, newline included: `prefix`, then
/// `'grok'` and each word quoted (atpkg `yolo::echo_line` and `default_echo_line`, and the
/// launcher's `printf` loop, byte for byte).
#[must_use]
pub fn echo_line(prefix: &str, args: &[OsString]) -> Vec<u8> {
    let mut line = prefix.as_bytes().to_vec();
    quote_into(&mut line, PROGRAM.as_bytes());
    for arg in args {
        line.push(b' ');
        quote_into(&mut line, arg.as_bytes());
    }
    line.push(b'\n');
    line
}

#[cfg(test)]
#[path = "yolo_tests.rs"]
mod tests;
