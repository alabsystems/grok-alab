// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SELF-UPDATE INTERCEPT, CARRIED FROM ATERM'S LAUNCHER (aterm
//! `docs/DESIGN-harness-interface-2026-11-12.md` §2.1, "The launcher keeps its other duties"):
//! `grok update` runs `aterm pkg update grok`, so the build that runs is always one aterm's
//! package manager verified, never one Grok Build fetched for itself.
//!
//! aterm's `grok` launcher reads its first word exactly as typed (`case "$1" in update)`,
//! aterm `crates/atpkg/src/platform/mod.rs`, `sh_selfupdate_prelude`, from the `grok` row's
//! `self_update_verbs` in `crates/atpkg/src/vendor_direct/table.rs`) and hands that launch to
//! atpkg's `__selfupdate`, which sorts its shape (`crates/atpkg/src/selfupdate.rs`, `classify`;
//! `crates/atpkg/src/cli.rs`, `selfupdate_plan`): `-h` or `--help` ahead of a literal `--` is
//! the vendor's own help after a line saying what the verb does here; the verb alone is the
//! managed check; anything after it is a shape aterm declines (exit 2). The check
//! (`cli.rs`, `selfupdate_check`) prints its announcement, runs the update as its child
//! ([`child_args`], with [`SPAWNER_PID_ENV`] set to its own pid), waits for it, and ends with
//! [`settle`]'s status and line. Once this harness leads the word the launcher runs
//! `grok-alab run -- "$@"` and deletes its block, so [`intercept`] sorts the same shapes here,
//! first, before anything else `run` does, and the lines are atpkg's, byte for byte, its
//! `atpkg:` included, as the yolo block's line keeps the launcher's `aterm:` (Decision 60).
//!
//! The intercept reads the first word only, as the launcher does. `update` after root options
//! or after the prompt, where clap still reads it as the subcommand, is the harness's own
//! refusal ([`crate::run`], Decision 5).

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;

/// The vendor's self-update verbs, exactly as typed as the first word (the row's
/// `self_update_verbs`).
pub const VERBS: &[&str] = &["update"];

/// How long the update waits for another pass's hold on the store, in seconds: atpkg's
/// `selfupdate::WAIT_LOCK_SECS`, aterm `crates/aterm-update-core/src/pkg_check.rs`
/// `PASS_WAIT_LOCK_SECS` (30 min), where a command a person types waits 10.
pub const WAIT_LOCK_SECS: u64 = 30 * 60;

/// The variable the update is started with, naming the process that waits for it (atpkg
/// `cli::SPAWNER_PID_ENV`): the waiter then stands down once that process is gone, and leaves
/// the line for a store held past [`WAIT_LOCK_SECS`] to it ([`contended_line`]).
pub const SPAWNER_PID_ENV: &str = "ATPKG_SPAWNER_PID";

/// The update's exit status when another pass held the store past [`WAIT_LOCK_SECS`] (atpkg
/// `lock::CONTENDED_EXIT`).
pub const CONTENDED_EXIT: u8 = 75;

/// The words after `aterm` that update grok through aterm's package manager: atpkg's
/// `selfupdate::child_argv` for a binary that is `aterm` (`pkg update grok --wait-lock 1800`).
#[must_use]
pub fn child_args() -> Vec<String> {
    vec![
        "pkg".to_string(),
        "update".to_string(),
        crate::yolo::PROGRAM.to_string(),
        "--wait-lock".to_string(),
        WAIT_LOCK_SECS.to_string(),
    ]
}

/// The line ahead of the vendor's own help for `grok update --help` (atpkg
/// `selfupdate::help_line`).
pub const HELP_LINE: &str = "atpkg: here `grok update` runs `aterm pkg update grok` (`aterm help \
                             pkg`) \u{2014} the vendor's own help follows";

/// What the intercept makes of a launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intercept {
    /// The first word is no self-update verb: the launch goes on.
    Pass,
    /// `grok update` alone: `aterm pkg update grok --wait-lock 1800` runs as the harness's child.
    Update,
    /// `-h` or `--help` ahead of a literal `--`: grok's own help, after [`HELP_LINE`].
    Help,
    /// Any other shape: refused with this line, exit 2.
    Declined(String),
}

/// Sort the launch `args` (after `run --`) as the launcher's block and atpkg's `classify` sort
/// it for the `grok` row.
#[must_use]
pub fn intercept(args: &[OsString]) -> Intercept {
    let Some((verb, rest)) = args.split_first() else {
        return Intercept::Pass;
    };
    if !crate::yolo::listed(VERBS, verb.as_bytes()) {
        return Intercept::Pass;
    }
    if args
        .iter()
        .take_while(|a| a.as_bytes() != b"--")
        .any(|a| a == "-h" || a == "--help")
    {
        return Intercept::Help;
    }
    if rest.is_empty() {
        Intercept::Update
    } else {
        Intercept::Declined(declined_line(verb, rest))
    }
}

/// The longest echo of typed text a line carries, in bytes; the rest is `…` (atpkg
/// `selfupdate::ECHO_CAP`).
const ECHO_CAP: usize = 64;

/// A typed word as the lines echo it: plain when every byte is printable ASCII without
/// whitespace, else `{:?}`-escaped, so nothing the terminal would interpret is written (atpkg
/// `selfupdate::echo_token`).
fn echo_token(word: &OsString) -> String {
    let bytes = word.as_bytes();
    match word.to_str() {
        Some(s) if !bytes.is_empty() && bytes.iter().all(|b| (0x21..=0x7e).contains(b)) => {
            s.to_string()
        }
        _ => format!("{:?}", word.to_string_lossy()),
    }
}

/// The typed words echoed, space-joined, capped at [`ECHO_CAP`] bytes on a char boundary with
/// `…` (atpkg `selfupdate::echo_words`).
fn echo_words(words: &[OsString]) -> String {
    let joined = words.iter().map(echo_token).collect::<Vec<_>>().join(" ");
    if joined.len() <= ECHO_CAP {
        return joined;
    }
    let mut end = ECHO_CAP;
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    let mut capped = joined.get(..end).unwrap_or_default().to_string();
    capped.push('…');
    capped
}

/// The refusal of `grok <verb> <rest…>` (atpkg `selfupdate::declined_line`, `Decline::Shape`,
/// for the `grok` row: its product is updated from xAI).
#[must_use]
pub fn declined_line(verb: &OsString, rest: &[OsString]) -> String {
    let mut typed = vec![verb.clone()];
    typed.extend(rest.iter().cloned());
    format!(
        "atpkg: `grok {}` not run \u{2014} not a shape aterm's updater takes; `grok {}` alone \
         checks xAI's latest now",
        echo_words(&typed),
        echo_token(verb)
    )
}

/// The line ahead of `aterm pkg update grok`, naming the build that runs now (`have`: its
/// version, `build <id>`, or `(no version recorded)` with none current; atpkg
/// `selfupdate::announce_line`).
#[must_use]
pub fn announce_line(have: &str) -> String {
    format!(
        "atpkg: aterm updates Grok Build from xAI \u{2014} you have {have}; checking now: \
         `aterm pkg update grok`"
    )
}

/// The line when the update could not be started (atpkg `selfupdate::could_not_run_line`; the
/// launch then exits 126).
#[must_use]
pub fn could_not_run_line(aterm: &std::path::Path, err: &std::io::Error) -> String {
    format!(
        "atpkg: could not run {} update grok: {err} \u{2014} `aterm pkg update grok` does the \
         same check",
        aterm.display()
    )
}

/// The line after an update that exited non-zero, `build` the build current now (atpkg
/// `selfupdate::incomplete_line`).
#[must_use]
pub fn incomplete_line(build: &str) -> String {
    format!("atpkg: grok {build} stays; `grok update` tries again")
}

/// The line after an update a signal stopped (atpkg `selfupdate::stopped_line`).
#[must_use]
pub fn stopped_line(build: &str) -> String {
    format!(
        "atpkg: the grok check was stopped before it finished \u{2014} grok {build} stays; `grok \
         update` tries again"
    )
}

/// The line after an update that found the store held past [`WAIT_LOCK_SECS`], by a person's
/// own `aterm pkg` command or by another update (atpkg `selfupdate::contended_line`).
#[must_use]
pub fn contended_line(build: &str, holder_is_person: bool) -> String {
    let holder = if holder_is_person {
        "another `aterm pkg` command"
    } else {
        "another update"
    };
    format!(
        "atpkg: {holder} is still running after {} \u{2014} grok {build} stays; `grok update` \
         again when it finishes",
        wait_words(WAIT_LOCK_SECS)
    )
}

/// A wait in seconds below two minutes, else in minutes and any seconds left (atpkg
/// `selfupdate::wait_words`).
#[must_use]
pub fn wait_words(secs: u64) -> String {
    if secs < 120 {
        return format!("{secs} s");
    }
    if secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else {
        format!("{} min {} s", secs / 60, secs % 60)
    }
}

/// How `grok update` ends once the update exited with `code` (`None`: a signal stopped it), as
/// atpkg's `selfupdate_check` ends it: 0 and 2 pass on alone; [`CONTENDED_EXIT`] passes on
/// after [`contended_line`]; a signal is 1 after [`stopped_line`], and any other status 1 after
/// [`incomplete_line`]. `build` (the build current now) and `holder_is_person` (whether a
/// person's `aterm pkg` command holds the store) are read only for the line that names them.
#[must_use]
pub fn settle(
    code: Option<i32>,
    build: impl FnOnce() -> String,
    holder_is_person: impl FnOnce() -> bool,
) -> (u8, Option<String>) {
    match code {
        Some(0) => (0, None),
        Some(2) => (2, None),
        Some(c) if c == i32::from(CONTENDED_EXIT) => (
            CONTENDED_EXIT,
            Some(contended_line(&build(), holder_is_person())),
        ),
        None => (1, Some(stopped_line(&build()))),
        Some(_) => (1, Some(incomplete_line(&build()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    /// atpkg's `grok_and_kimi_check_on_their_bare_verbs_only` for the `grok` row: the bare verb
    /// is the managed check, anything after it a declined shape, `upgrade` (not grok's verb)
    /// and a verb behind an option pass through; help is help wherever it stands before `--`.
    #[test]
    fn only_the_bare_verb_updates_and_help_is_the_vendors() {
        assert_eq!(intercept(&argv(&["update"])), Intercept::Update);
        for shape in [&["update", "--check"][..], &["update", "1.0.70"]] {
            assert!(
                matches!(intercept(&argv(shape)), Intercept::Declined(_)),
                "{shape:?}"
            );
        }
        for pass in [
            &[][..],
            &["upgrade"],
            &["--resume", "update"],
            &["fix it", "update"],
            &["Update"],
        ] {
            assert_eq!(intercept(&argv(pass)), Intercept::Pass, "{pass:?}");
        }
        for help in [
            &["update", "--help"][..],
            &["update", "-h"],
            &["update", "x", "--help"],
        ] {
            assert_eq!(intercept(&argv(help)), Intercept::Help, "{help:?}");
        }
        assert!(matches!(
            intercept(&argv(&["update", "--", "--help"])),
            Intercept::Declined(_)
        ));
    }

    /// atpkg's own sentences for the `grok` row (`selfupdate.rs`), byte for byte, `atpkg:`
    /// included.
    #[test]
    fn the_lines_are_atpkgs_sentences() {
        assert_eq!(
            declined_line(&"update".into(), &argv(&["--check"])),
            "atpkg: `grok update --check` not run \u{2014} not a shape aterm's updater takes; \
             `grok update` alone checks xAI's latest now"
        );
        assert_eq!(
            declined_line(&"update".into(), &argv(&["a b", &"x".repeat(80)])),
            format!(
                "atpkg: `grok update \"a b\" {}…` not run \u{2014} not a shape aterm's updater \
                 takes; `grok update` alone checks xAI's latest now",
                "x".repeat(64 - "update \"a b\" ".len())
            )
        );
        assert_eq!(
            announce_line("1.0.70"),
            "atpkg: aterm updates Grok Build from xAI \u{2014} you have 1.0.70; checking now: \
             `aterm pkg update grok`"
        );
        assert_eq!(
            HELP_LINE,
            "atpkg: here `grok update` runs `aterm pkg update grok` (`aterm help pkg`) \u{2014} \
             the vendor's own help follows"
        );
        assert_eq!(
            could_not_run_line(
                std::path::Path::new("/opt/bin/aterm"),
                &std::io::Error::from_raw_os_error(libc::EACCES)
            ),
            format!(
                "atpkg: could not run /opt/bin/aterm update grok: {} \u{2014} `aterm pkg update \
                 grok` does the same check",
                std::io::Error::from_raw_os_error(libc::EACCES)
            )
        );
        assert_eq!(
            incomplete_line("1.0.70"),
            "atpkg: grok 1.0.70 stays; `grok update` tries again"
        );
        assert_eq!(
            stopped_line("(no version recorded)"),
            "atpkg: the grok check was stopped before it finished \u{2014} grok (no version \
             recorded) stays; `grok update` tries again"
        );
        assert_eq!(
            contended_line("1.0.70", false),
            "atpkg: another update is still running after 30 min \u{2014} grok 1.0.70 stays; \
             `grok update` again when it finishes"
        );
        assert!(contended_line("1.0.70", true).starts_with(
            "atpkg: another `aterm pkg` command is still running after 30 min \u{2014}"
        ));
        assert_eq!(VERBS, [crate::lanes::SELF_UPDATE_COMMAND]);
    }

    /// atpkg's `child_argv` for `aterm`, its `wait_words`, and `selfupdate_check`'s ending: the
    /// status passed on, and the line after it read only when it is printed.
    #[test]
    fn the_update_runs_as_atpkg_runs_it_and_ends_as_it_ends() {
        assert_eq!(
            child_args(),
            ["pkg", "update", "grok", "--wait-lock", "1800"]
        );
        assert_eq!(SPAWNER_PID_ENV, "ATPKG_SPAWNER_PID");
        for (secs, words) in [
            (0, "0 s"),
            (119, "119 s"),
            (120, "2 min"),
            (150, "2 min 30 s"),
            (1800, "30 min"),
        ] {
            assert_eq!(wait_words(secs), words, "{secs}");
        }
        let unread = || -> String { panic!("the build was read for no line") };
        let unasked = || -> bool { panic!("the holder was read for no line") };
        assert_eq!(settle(Some(0), unread, unasked), (0, None));
        assert_eq!(settle(Some(2), unread, unasked), (2, None));
        let build = || "1.0.71".to_string();
        assert_eq!(
            settle(Some(75), build, || true),
            (75, Some(contended_line("1.0.71", true)))
        );
        assert_eq!(
            settle(None, build, unasked),
            (1, Some(stopped_line("1.0.71")))
        );
        for code in [1, 3, 74, 76, 126, 127] {
            assert_eq!(
                settle(Some(code), build, unasked),
                (1, Some(incomplete_line("1.0.71"))),
                "{code}"
            );
        }
    }
}
