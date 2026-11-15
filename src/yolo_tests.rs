// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launcher's yolo decision for `grok`, case for case: every `grok` case of aterm's
//! `crates/atpkg/src/yolo_tests.rs` (`grok_and_kimi_take_their_one_flag_where_their_parsers_read_it`
//! and `every_row_spells_its_yolo_policy_in_full`), whose answers this module must give.

use super::*;

fn argv(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

fn d(words: &[&str]) -> Launch {
    decide(&argv(words))
}

#[test]
fn the_flag_lands_where_groks_parser_reads_it() {
    assert_eq!(FLAGS, ["--always-approve"]);
    assert_eq!(AFTER, ["agent"]);
    assert_eq!(d(&[]), Launch::Default(argv(&["--always-approve"])));
    assert_eq!(
        d(&["-m", "grok-4", "fix it"]),
        Launch::Default(argv(&["--always-approve", "-m", "grok-4", "fix it"]))
    );
    assert_eq!(
        d(&["--resume", "abc", "agent", "--json"]),
        Launch::Default(argv(&[
            "--resume",
            "abc",
            "agent",
            "--always-approve",
            "--json"
        ]))
    );
    assert_eq!(
        d(&["agent", "-p", "hi"]),
        Launch::Default(argv(&["agent", "--always-approve", "-p", "hi"]))
    );
    assert_eq!(
        d(&["--yolo", "agent", "-p", "hi"]),
        Launch::Flag(argv(&["agent", "--always-approve", "-p", "hi"]))
    );
    assert_eq!(
        d(&["--cwd", "/w", "--yolo"]),
        Launch::Flag(argv(&["--cwd", "/w", "--always-approve"]))
    );
}

#[test]
fn a_launch_that_decides_its_posture_or_names_a_management_word_runs_as_typed() {
    for decided in [
        &["--always-approve"][..],
        &["--permission-mode", "plan"],
        &["--permission-mode=default"],
        &["--sandbox=strict"],
        &["--allow", "Bash(ls)"],
        &["--disallowed-tools=Bash"],
        &["agent", "--dangerously-skip-permissions"],
        &["-m", "grok-4", "--deny", "Write"],
    ] {
        assert_eq!(d(decided), Launch::AsTyped, "{decided:?}");
    }
    for word in [
        "login",
        "mcp",
        "models",
        "doctor",
        "update",
        "version",
        "worktree",
        "worker",
        "cursor-worker",
        "clone",
        "ssh",
        "host-server",
        "help",
    ] {
        assert_eq!(d(&[word]), Launch::AsTyped, "{word}");
    }
    assert_eq!(
        d(&["--debug", "worker", "start"]),
        Launch::AsTyped,
        "a management word behind a known switch runs as typed too"
    );
}

#[test]
fn every_leading_option_is_read_by_its_arity_and_the_flag_still_goes_in_front() {
    for typed in [
        &["--effort", "high", "fix it"][..],
        &["--reasoning-effort=high", "fix it"],
        &["--add-dir", "/x", "fix it"],
        &["-s", "6f1c", "fix it"],
        &["--session-id", "6f1c"],
        &["--rules", "be terse", "fix it"],
        &["--system-prompt", "x"],
        &["--tools", "read_file", "--max-turns", "3", "fix it"],
        &["--agent", "agent"],
        &["--output-format", "json", "-p", "hi"],
        &["--debug", "fix it"],
        &["--minimal"],
        &["--no-alt-screen", "--fullscreen", "--verbatim"],
        &["-c", "--fork-session"],
        &["-w"],
        &["-w", "feat", "fix it"],
        &["-w", "login"],
        &["-wfeat", "--ref", "main"],
        &["--worktree=feat"],
        &["-r"],
        &["--resume", "agent"],
        &["-r", "--no-plan"],
        &[
            "--leader-socket",
            "/Users/_owner/.grok/leader-work-99068163.sock",
            "--resume",
            "abc",
        ],
    ] {
        let mut want = argv(&["--always-approve"]);
        want.extend(argv(typed));
        assert_eq!(d(typed), Launch::Default(want), "{typed:?}");
    }
    assert_eq!(
        d(&["-w", "feat", "agent", "-p", "hi"]),
        Launch::Default(argv(&[
            "-w",
            "feat",
            "agent",
            "--always-approve",
            "-p",
            "hi"
        ]))
    );
}

#[test]
fn a_typed_yolo_is_the_flag_unless_removing_it_would_change_the_launch() {
    assert_eq!(
        d(&["-w", "--yolo"]),
        Launch::Flag(argv(&["-w", "--always-approve"]))
    );
    for typed in [
        &["-w", "--yolo", "agent", "-p", "hi"][..],
        &["-r", "--yolo", "fix it"],
        &["--worktree", "--yolo", "--yolo", "agent"],
    ] {
        assert_eq!(
            d(typed),
            Launch::AsTyped,
            "{typed:?}: dropping the word would hand the next one to the optional value"
        );
    }
    assert_eq!(
        d(&[
            "--yolo",
            "--dangerously-skip-permissions",
            "agent",
            "-p",
            "x"
        ]),
        Launch::Flag(argv(&["agent", "--always-approve", "-p", "x"])),
        "grok's own alias of the flag is dropped: its parser refuses one option twice"
    );
    assert_eq!(
        d(&["--yolo", "--always-approve", "fix it"]),
        Launch::Flag(argv(&["--always-approve", "fix it"])),
        "a flag already typed is not added twice"
    );
    assert_eq!(
        d(&["--yolo", "--", "--yolo"]),
        Launch::Flag(argv(&["--always-approve", "--", "--yolo"])),
        "the words after `--` are the prompt's"
    );
    assert_eq!(d(&["--unknown", "x"]), Launch::AsTyped);
    assert_eq!(
        d(&["--unknown", "--yolo"]),
        Launch::AsTyped,
        "an after word may hide behind an option this scan does not know"
    );
    assert_eq!(d(&["--model"]), Launch::AsTyped, "a value is missing");
}

#[test]
fn a_word_that_is_not_utf8_is_carried() {
    use std::os::unix::ffi::OsStringExt as _;
    let odd = OsString::from_vec(vec![b'f', 0xff, b'x']);
    assert_eq!(
        decide(std::slice::from_ref(&odd)),
        Launch::Default(vec![OsString::from("--always-approve"), odd.clone()])
    );
    let mut line = DEFAULT_ECHO_PREFIX.as_bytes().to_vec();
    line.extend_from_slice(b"'grok' '--always-approve' 'f\xffx'\n");
    assert_eq!(
        echo_line(
            DEFAULT_ECHO_PREFIX,
            &[OsString::from("--always-approve"), odd]
        ),
        line
    );
}

#[test]
fn no_harness_is_stripped_ahead_of_a_literal_double_dash_only() {
    assert_eq!(strip_no_harness(&argv(&["-m", "grok-4"])), None);
    assert_eq!(
        strip_no_harness(&argv(&["--no-harness", "-m", "grok-4", "fix it"])),
        Some(argv(&["-m", "grok-4", "fix it"]))
    );
    assert_eq!(
        strip_no_harness(&argv(&["--no-alab-harness", "--", "--no-harness"])),
        Some(argv(&["--", "--no-harness"]))
    );
    assert_eq!(strip_no_harness(&argv(&["--", "--no-harness"])), None);
}

/// The line the launcher's `printf` loop writes: the prefix, `'grok'`, then every word
/// single-quoted with `'` spelled `'\''` (atpkg `default_echo_line` and `echo_line`).
#[test]
fn the_echo_is_the_launchers_line_byte_for_byte() {
    assert_eq!(
        String::from_utf8(echo_line(
            DEFAULT_ECHO_PREFIX,
            &argv(&["--always-approve", "-m", "grok-4", "it's"])
        ))
        .unwrap(),
        "aterm: yolo (default): running 'grok' '--always-approve' '-m' 'grok-4' 'it'\\''s'\n"
    );
    assert_eq!(
        String::from_utf8(echo_line(ECHO_PREFIX, &argv(&["--always-approve"]))).unwrap(),
        "aterm: --yolo: running 'grok' '--always-approve'\n"
    );
    assert_eq!(d(&[]).echo_prefix(), Some(DEFAULT_ECHO_PREFIX));
    assert_eq!(d(&["--yolo"]).echo_prefix(), Some(ECHO_PREFIX));
    assert_eq!(d(&["login"]).echo_prefix(), None);
}

#[test]
fn every_marker_matches_itself_and_only_its_own_spellings() {
    assert!(marker_matches("--sandbox", b"--sandbox"));
    assert!(marker_matches("--sandbox", b"--sandbox=x"));
    assert!(!marker_matches("--sandbox", b"--sandboxed"));
    assert!(marker_matches("--sandbox=", b"--sandbox="));
    assert!(!marker_matches("--sandbox=", b"--sandbox"));
    assert!(marker_matches("-m", b"-mgrok-4"));
    assert!(!marker_matches("-m", b"--model"));
    for flag in FLAGS {
        assert!(MARKERS.contains(flag), "{flag} decides the posture");
    }
}
