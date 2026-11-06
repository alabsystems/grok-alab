// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const ID: &str = "0b5c1f3e-8d2a-4c7b-9e61-2f4a6d8c0b13";
const FRESH: &str = "7f3a9c21-4b6e-4d8f-a1c2-3e5f7a9b1d24";

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn plan_of(args: &[&str]) -> Plan {
    plan(&os(args), Some(FRESH.to_string()))
}

fn carried(args: &[&str]) -> Carried {
    match plan_of(args) {
        Plan::Session(c) => c,
        other => panic!("{args:?}: not carried: {other:?}"),
    }
}

fn uncarried(args: &[&str]) -> String {
    match plan_of(args) {
        Plan::Uncarried { why } => why,
        other => panic!("{args:?}: not uncarried: {other:?}"),
    }
}

fn passthrough(args: &[&str]) -> (String, Option<String>) {
    match plan_of(args) {
        Plan::PassThrough { why, command } => (why, command),
        other => panic!("{args:?}: not pass-through: {other:?}"),
    }
}

#[test]
fn a_fresh_session_is_started_under_an_id_the_harness_made() {
    let c = carried(&[]);
    assert_eq!(c.session, FRESH);
    assert_eq!(c.injected, ["--session-id", FRESH]);
    assert!(c.kept.is_empty());
    assert_eq!(c.relaunch_argv(), ["--resume", FRESH]);
}

#[test]
fn the_first_prompt_is_never_typed_again() {
    let c = carried(&["--model", "grok-code-fast-1", "fix the failing test"]);
    assert_eq!(c.kept, ["--model", "grok-code-fast-1"]);
    assert_eq!(
        c.relaunch_argv(),
        ["--model", "grok-code-fast-1", "--resume", FRESH]
    );
    let c = carried(&[
        "--always-approve",
        "--",
        "--a prompt that looks like a flag",
    ]);
    assert_eq!(c.relaunch_argv(), ["--always-approve", "--resume", FRESH]);
}

#[test]
fn the_resume_pair_follows_the_flags_as_atems_pool_line_orders_it() {
    // relaunch.rs `pool_line`: Claude, Grok and Kimi put their resume pair after the flags.
    let c = carried(&[
        "--permission-mode",
        "acceptEdits",
        "--deny",
        "Bash(rm:*)",
        "--sandbox=strict",
        "-m",
        "grok-4",
    ]);
    assert_eq!(
        c.relaunch_argv(),
        [
            "--permission-mode",
            "acceptEdits",
            "--deny",
            "Bash(rm:*)",
            "--sandbox=strict",
            "-m",
            "grok-4",
            "--resume",
            FRESH
        ]
    );
    assert_eq!(resume_argv(ID), ["--resume", ID]);
}

#[test]
fn the_posture_the_person_launched_with_survives_a_relaunch() {
    for posture in [
        &["--always-approve"][..],
        &["--dangerously-skip-permissions"],
        &["--permission-mode", "plan"],
        &["--allow", "Read"],
        &["--allowedTools", "Read"],
        &["--allowed-tools=Read"],
        &["--deny", "Write"],
        &["--disallowedTools", "Write"],
        &["--disallowed-tools", "web_search"],
        &["--tools", "read,grep"],
        &["--sandbox", "workspace-write"],
        &["--no-subagents"],
        &["--disable-web-search"],
    ] {
        let c = carried(posture);
        assert_eq!(&c.kept[..], posture, "{posture:?}");
    }
}

#[test]
fn a_launch_that_names_its_session_by_id_keeps_it_and_gets_nothing_injected() {
    let attached = format!("--resume={ID}");
    let attached_id = format!("--session-id={ID}");
    for args in [
        vec!["--resume", ID],
        vec!["-r", ID],
        vec![attached.as_str()],
        vec!["--session-id", ID],
        vec!["-s", ID],
        vec![attached_id.as_str()],
    ] {
        let c = carried(&args);
        assert_eq!(c.session, ID, "{args:?}");
        assert!(c.injected.is_empty(), "{args:?}");
        assert!(c.kept.is_empty(), "{args:?}");
        assert_eq!(c.relaunch_argv(), ["--resume", ID], "{args:?}");
    }
}

#[test]
fn a_session_the_harness_cannot_name_is_never_carried() {
    assert!(uncarried(&["--continue"]).contains("--continue"));
    assert!(uncarried(&["-c"]).contains("--continue"));
    assert!(uncarried(&["--resume"]).contains("no id"));
    assert!(uncarried(&["-r", "--model", "x"]).contains("no id"));
    assert!(uncarried(&["--resume", "my refactor"]).contains("title"));
    assert!(uncarried(&["--resume", ID, "--fork-session"]).contains("fork"));
    assert!(uncarried(&["--session-id", "not-a-uuid"]).contains("UUID"));
    assert!(uncarried(&["--resume", ID, "--session-id", FRESH]).contains("two session"));
    assert!(uncarried(&["-s", ID, "-r", ID]).contains("two session"));
}

#[test]
fn a_worktree_launch_is_never_carried() {
    for args in [
        &["--worktree"][..],
        &["-w"],
        &["-w", "feature-x"],
        &["--worktree=feature-x"],
        &["--worktree-ref", "main"],
        &["--ref", "main"],
    ] {
        assert!(uncarried(args).contains("worktree"), "{args:?}");
    }
}

#[test]
fn restore_code_and_verbatim_are_dropped_from_the_relaunch() {
    let c = carried(&["--resume", ID, "--restore-code"]);
    assert_eq!(c.relaunch_argv(), ["--resume", ID]);
    let c = carried(&["--verbatim", "exact prompt"]);
    assert_eq!(c.relaunch_argv(), ["--resume", FRESH]);
}

#[test]
fn subcommands_help_version_and_one_shot_prompts_pass_through() {
    for (args, cmd) in [
        (&["sessions", "list"][..], Some("sessions")),
        (&["update"], Some("update")),
        (&["--debug", "login", "--device-auth"], Some("login")),
        (&["agent", "--model", "x"], Some("agent")),
        (&["version"], Some("version")),
        (&["dashboard"], Some("dashboard")),
        (&["--help"], None),
        (&["-h"], None),
        (&["--version"], None),
        (&["-v"], None),
        (&["-p", "what is 2+2"], None),
        (&["--single", "hi"], None),
        (&["--prompt-file", "p.txt"], None),
        (&["--prompt-json", "[]"], None),
        (&["--model", "x", "-p", "hi"], None),
    ] {
        let (_, command) = passthrough(args);
        assert_eq!(command.as_deref(), cmd, "{args:?}");
    }
    assert_eq!(command(&plan_of(&["update"])), Some(SELF_UPDATE_COMMAND));
    assert_eq!(command(&plan_of(&["--help"])), None);
}

#[test]
fn a_subcommand_after_the_prompt_is_still_the_subcommand() {
    // clap reads `grok [PROMPT] [COMMAND]`: `grok "hi" update` runs `update` with a prompt.
    for (args, cmd) in [
        (&["fix the test", "update"][..], "update"),
        (&["--model", "x", "hi", "update", "--force"], "update"),
        (&["hi", "sessions", "list"], "sessions"),
        (&["hi", "v"], "v"),
    ] {
        let (_, command) = passthrough(args);
        assert_eq!(command.as_deref(), Some(cmd), "{args:?}");
    }
    assert_eq!(
        command(&plan_of(&["fix the test", "update"])),
        Some(SELF_UPDATE_COMMAND)
    );
    // An option's value and the words after `--` are never a subcommand.
    assert_eq!(carried(&["--rules", "update"]).kept, ["--rules", "update"]);
    assert!(uncarried(&["--", "update"]).contains("after `--`"));
    assert!(uncarried(&["-w", "update"]).contains("worktree"));
}

#[test]
fn an_unknown_option_or_shape_refuses_to_carry_the_session() {
    assert!(uncarried(&["--no-such-flag"]).contains("--no-such-flag"));
    assert!(uncarried(&["-mgrok-4"]).contains("-mgrok-4"));
    assert!(uncarried(&["-cr"]).contains("-cr"));
    assert!(uncarried(&["--always-approve=yes"]).contains("takes no value"));
    assert!(uncarried(&["--model"]).contains("no value"));
    assert!(uncarried(&["first prompt", "second"]).contains("second positional"));
    assert!(uncarried(&["--", "a", "b"]).contains("after `--`"));
    assert!(uncarried(&["prompt", "--", "more"]).contains("after `--`"));
    assert!(uncarried(&["--", "sessions"]).contains("after `--`"));
}

#[test]
fn a_value_that_looks_like_a_flag_is_still_the_value_of_a_one_value_option() {
    let c = carried(&["--rules", "--be-terse", "--add-dir", "-weird-dir"]);
    assert_eq!(c.kept, ["--rules", "--be-terse", "--add-dir", "-weird-dir"]);
}

#[test]
fn a_non_utf8_argument_is_run_as_typed_and_never_carried() {
    use std::os::unix::ffi::OsStringExt;
    let args = vec![OsString::from_vec(vec![0x66, 0xff, 0x6f])];
    assert!(matches!(
        plan(&args, Some(FRESH.to_string())),
        Plan::Uncarried { .. }
    ));
}

#[test]
fn no_session_id_means_no_carry() {
    assert!(matches!(plan(&[], None), Plan::Uncarried { .. }));
    assert!(matches!(
        plan(&[], Some("nope".to_string())),
        Plan::Uncarried { .. }
    ));
}

#[test]
fn a_session_uuid_is_8_4_4_4_12_hex() {
    assert!(is_session_uuid(ID));
    assert!(is_session_uuid("0B5C1F3E-8D2A-4C7B-9E61-2F4A6D8C0B13"));
    for bad in [
        "",
        "0b5c1f3e8d2a4c7b9e612f4a6d8c0b13",
        "0b5c1f3e-8d2a-4c7b-9e61-2f4a6d8c0b1",
        "0b5c1f3e-8d2a-4c7b-9e61-2f4a6d8c0b13a",
        "0b5c1f3e_8d2a-4c7b-9e61-2f4a6d8c0b13",
        "zb5c1f3e-8d2a-4c7b-9e61-2f4a6d8c0b13",
    ] {
        assert!(!is_session_uuid(bad), "{bad:?}");
    }
}

#[test]
fn every_measured_option_is_in_the_table_once() {
    let mut names: Vec<&str> = FLAGS.iter().map(|(n, _, _)| *n).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "a flag is listed twice");
    // Every long option 1.0.70's --help prints (measured 2026-10-24).
    for long in [
        "--add-dir",
        "--agent",
        "--agents",
        "--allow",
        "--always-approve",
        "--continue",
        "--cwd",
        "--debug",
        "--debug-file",
        "--deny",
        "--disable-web-search",
        "--disallowed-tools",
        "--fork-session",
        "--fullscreen",
        "--help",
        "--include-partial-messages",
        "--json-schema",
        "--leader-socket",
        "--model",
        "--max-turns",
        "--minimal",
        "--no-alt-screen",
        "--no-plan",
        "--no-subagents",
        "--oauth",
        "--output-format",
        "--single",
        "--permission-mode",
        "--prompt-file",
        "--prompt-json",
        "--resume",
        "--reasoning-effort",
        "--restore-code",
        "--rules",
        "--session-id",
        "--sandbox",
        "--system-prompt-override",
        "--tools",
        "--version",
        "--verbatim",
        "--worktree",
        "--worktree-ref",
    ] {
        assert!(flag(long).is_some(), "{long} missing");
    }
}
