// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `grok-alab run -- <words>`, the command line aterm's `grok` launcher runs once this harness
//! leads the word, carries the launcher's blocks in the launcher's order (aterm
//! `docs/DESIGN-harness-interface-2026-11-12.md` §2.1, "The launcher keeps its other duties"):
//! one test per block, named after what it does, as kimi-alab names its own.

mod common;

use std::process::{Output, Stdio};

use common::Rig;

const V1: (u64, u64, u64) = (1, 0, 70);

/// The line aterm's launcher prints ahead of a launch its yolo block changed.
const DEFAULT_ECHO: &str = "aterm: yolo (default): running 'grok'";
const FLAG_ECHO: &str = "aterm: --yolo: running 'grok'";

/// The aterm verb the sign-in gate runs, as the fake `aterm` records it.
const SIGNIN: &str = "harness signin --cli grok --wait\n";

/// A rig with build 1.0.70 current.
fn rig_v1(name: &str) -> (Rig, u64) {
    let rig = Rig::new(name);
    let id = rig.install(V1);
    rig.make_current(id);
    (rig, id)
}

/// `grok-alab run -- <typed>` to its end with no terminal, the fake grok told to exit at once,
/// `env` set over the rig's: what it wrote, and the harness's pid.
fn run(rig: &Rig, typed: &[&str], env: &[(&str, &str)]) -> (Output, i32) {
    let mut words = vec!["run", "--"];
    words.extend_from_slice(typed);
    let mut cmd = rig.command(&words);
    cmd.env("FAKE_GROK_MODE", "exit:0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn the harness");
    let pid = i32::try_from(child.id()).unwrap();
    (child.wait_with_output().unwrap(), pid)
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The words grok was started with, less the `--session-id <uuid>` the harness puts in front
/// of a fresh session it carries (its own, not the launcher's).
fn launch_words(argv: &[String]) -> Vec<String> {
    match argv {
        [flag, id, rest @ ..]
            if flag == "--session-id" && grok_alab::lanes::is_session_uuid(id) =>
        {
            rest.to_vec()
        }
        _ => argv.to_vec(),
    }
}

/// `prefix` and each word single-quoted, as the launcher's `printf` loop writes them.
fn echo(prefix: &str, words: &[&str]) -> String {
    let mut line = prefix.to_string();
    for w in words {
        line.push_str(" '");
        line.push_str(&w.replace('\'', "'\\''"));
        line.push('\'');
    }
    line.push('\n');
    line
}

/// THE YOLO DEFAULT (aterm `crates/atpkg/src/yolo.rs`, `decide`, the `grok` row's `yolo_*`): a
/// launch that names no permission posture gets `--always-approve` in front (behind `agent`), a
/// typed `--yolo` becomes it, and each prints the launcher's line on stderr, byte for byte; a
/// launch that names its own posture or a management subcommand runs as typed and prints
/// nothing; `--no-harness` asks for grok alone, which gets no default and runs in the harness's
/// place.
#[test]
fn run_sets_the_yolo_default() {
    let (rig, _) = rig_v1("run-yolo");
    type Case = (
        &'static [&'static str],
        &'static [&'static str],
        Option<&'static str>,
    );
    let cases: [Case; 11] = [
        (&[], &["--always-approve"], Some(DEFAULT_ECHO)),
        (
            &["-m", "grok-4", "fix it"],
            &["--always-approve", "-m", "grok-4", "fix it"],
            Some(DEFAULT_ECHO),
        ),
        (&["it's"], &["--always-approve", "it's"], Some(DEFAULT_ECHO)),
        (
            &["agent", "-p", "hi"],
            &["agent", "--always-approve", "-p", "hi"],
            Some(DEFAULT_ECHO),
        ),
        (
            &["--yolo", "fix it"],
            &["--always-approve", "fix it"],
            Some(FLAG_ECHO),
        ),
        (
            &[
                "--yolo",
                "--dangerously-skip-permissions",
                "agent",
                "-p",
                "x",
            ],
            &["agent", "--always-approve", "-p", "x"],
            Some(FLAG_ECHO),
        ),
        (
            &["--permission-mode", "plan", "fix it"],
            &["--permission-mode", "plan", "fix it"],
            None,
        ),
        (
            &["--dangerously-skip-permissions", "fix it"],
            &["--dangerously-skip-permissions", "fix it"],
            None,
        ),
        (&["sessions", "list"], &["sessions", "list"], None),
        (&["--unknown", "x"], &["--unknown", "x"], None),
        (&["--no-harness", "fix it"], &["fix it"], None),
    ];
    for (typed, runs, line) in cases {
        let before = rig.starts().len();
        let (out, pid) = run(&rig, typed, &[]);
        assert_eq!(out.status.code(), Some(0), "{typed:?}: {out:?}");
        let starts = rig.starts();
        assert_eq!(starts.len(), before + 1, "{typed:?}: grok ran once");
        let start = &starts[before];
        assert_eq!(launch_words(&start.argv), runs, "{typed:?}");
        let want = line.map_or_else(String::new, |prefix| echo(prefix, runs));
        assert_eq!(stderr(&out), want, "{typed:?}");
        if typed.first() == Some(&"--no-harness") {
            assert_eq!(
                start.pid, pid,
                "{typed:?}: grok runs in the harness's place"
            );
        }
    }
}

/// THE SIGN-IN GATE (aterm `crates/atpkg/src/platform/mod.rs`, `sh_signin_prelude`, the `grok`
/// row's `signin_gate`): under aterm a signed-out interactive launch asks aterm to sign the
/// machine in first; a refusal exits 69 with one line and starts nothing, and prints no yolo
/// line (the gate comes first); a sign-in runs grok once, after the yolo line. Which launch is
/// gated is the launcher's rule, word for word: its first word alone decides, past the option
/// values it skips; an option it does not know is passed over; a one-shot option and
/// `--no-harness` are never gated. Outside aterm nothing is asked.
#[test]
fn run_gates_a_signed_out_launch() {
    let (rig, _) = rig_v1("run-gate");
    rig.sign_out();
    let calls = rig.fake_aterm();
    let called = || std::fs::read_to_string(&calls).unwrap_or_default();
    let forget = || {
        let _ = std::fs::remove_file(&calls);
    };
    let tab = ("ATERM_CHILD", "1");
    let refuses = ("FAKE_ATERM_EXIT", "1");

    let (out, _) = run(&rig, &["fix it"], &[tab, refuses]);
    assert_eq!(out.status.code(), Some(69), "{out:?}");
    let err = stderr(&out);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("grok was not started"), "{err}");
    assert_eq!(called(), SIGNIN);
    assert!(rig.starts().is_empty(), "grok ran: {}", rig.log_text());

    forget();
    let (out, _) = run(&rig, &["fix it"], &[tab]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(called(), SIGNIN);
    assert_eq!(
        stderr(&out),
        echo(DEFAULT_ECHO, &["--always-approve", "fix it"])
    );
    assert_eq!(rig.starts().len(), 1);
    assert_eq!(rig.starts()[0].argv, ["--always-approve", "fix it"]);

    for (typed, asked) in [
        (&["fix it", "login"][..], true),
        (&["-r", "login"][..], true),
        (&["dashboard"][..], true),
        (&["--", "-p"][..], true),
        (&["--unknown", "login"][..], false),
        (&["-m", "grok-4", "doctor"][..], false),
        (&["fix it", "-p", "hi"][..], false),
        (&["--print=hi"][..], false),
        (&["--no-harness", "fix it"][..], false),
    ] {
        forget();
        let before = rig.starts().len();
        let (out, _) = run(&rig, typed, &[tab, refuses]);
        if asked {
            assert_eq!(out.status.code(), Some(69), "{typed:?}: {out:?}");
            assert_eq!(called(), SIGNIN, "{typed:?}");
            assert_eq!(rig.starts().len(), before, "{typed:?}: grok ran");
        } else {
            assert_eq!(out.status.code(), Some(0), "{typed:?}: {out:?}");
            assert_eq!(called(), "", "{typed:?}: aterm was asked");
            assert_eq!(rig.starts().len(), before + 1, "{typed:?}");
        }
    }
    forget();
    let (out, _) = run(&rig, &["fix it"], &[refuses]);
    assert_eq!(out.status.code(), Some(0), "outside aterm: {out:?}");
    assert_eq!(called(), "", "outside aterm nothing is asked");
}

/// THE SELF-UPDATE INTERCEPT (aterm `crates/atpkg/src/platform/mod.rs`, `sh_selfupdate_prelude`,
/// `crates/atpkg/src/selfupdate.rs`, `classify`, for the `grok` row's `self_update_verbs`, and
/// `crates/atpkg/src/cli.rs`, `selfupdate_check`): `grok update` runs `aterm pkg update grok
/// --wait-lock 1800` as the harness's child with `ATPKG_SPAWNER_PID` naming the harness, after
/// atpkg's line, whether or not a build is current, and ends as atpkg ends it (0 and 2 as they
/// are, 75 after the line naming the store's holder, any other status or a signal 1 after
/// atpkg's line); `update --help` is grok's own help after atpkg's line saying what the verb
/// does here, `--no-harness` dropped and exported as the launcher's shim does; any other shape
/// is refused (exit 2) and so is `update` with no aterm on `PATH`. grok's own updater never
/// runs, and `update` after the prompt is still refused (Decision 5).
#[test]
fn run_intercepts_self_update() {
    let (rig, _) = rig_v1("run-update");
    let calls = rig.fake_aterm();
    let spawners = calls.with_extension("calls.spawner");
    let called = || std::fs::read_to_string(&calls).unwrap_or_default();
    let spawner = || std::fs::read_to_string(&spawners).unwrap_or_default();
    let forget = || {
        let _ = std::fs::remove_file(&calls);
        let _ = std::fs::remove_file(&spawners);
    };
    const UPDATE: &str = "pkg update grok --wait-lock 1800\n";
    let announce = "atpkg: aterm updates Grok Build from xAI \u{2014} you have 1.0.70; checking \
                    now: `aterm pkg update grok`\n";
    let after = |line: &str| format!("{announce}atpkg: {line}\n");

    let (out, pid) = run(&rig, &["update"], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(called(), UPDATE);
    assert_eq!(spawner(), format!("{pid}\n"), "the harness waits for it");
    assert_eq!(stderr(&out), announce);
    assert!(rig.starts().is_empty(), "grok ran: {}", rig.log_text());

    let holder = rig.prefix.join("store.lock.holder");
    for (exit, holder_is_person, code, end) in [
        ("2", false, 2, None),
        (
            "75",
            false,
            75,
            Some(
                "another update is still running after 30 min \u{2014} grok 1.0.70 stays; \
                 `grok update` again when it finishes",
            ),
        ),
        (
            "75",
            true,
            75,
            Some(
                "another `aterm pkg` command is still running after 30 min \u{2014} grok 1.0.70 \
                 stays; `grok update` again when it finishes",
            ),
        ),
        (
            "1",
            false,
            1,
            Some("grok 1.0.70 stays; `grok update` tries again"),
        ),
        (
            "kill",
            false,
            1,
            Some(
                "the grok check was stopped before it finished \u{2014} grok 1.0.70 stays; \
                 `grok update` tries again",
            ),
        ),
    ] {
        forget();
        if holder_is_person {
            let me = std::process::id();
            std::fs::write(&holder, format!("pid={me}\nperson=1\n")).unwrap();
        }
        let (out, _) = run(&rig, &["update"], &[("FAKE_ATERM_EXIT", exit)]);
        let _ = std::fs::remove_file(&holder);
        assert_eq!(out.status.code(), Some(code), "{exit}: {out:?}");
        assert_eq!(called(), UPDATE, "{exit}");
        let want = end.map_or_else(|| announce.to_string(), after);
        assert_eq!(stderr(&out), want, "{exit}");
    }

    forget();
    let (out, _) = run(&rig, &["update", "--check"], &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(
        stderr(&out),
        "atpkg: `grok update --check` not run \u{2014} not a shape aterm's updater takes; \
         `grok update` alone checks xAI's latest now\n"
    );
    assert_eq!(called(), "");
    assert!(rig.starts().is_empty());

    let help = "atpkg: here `grok update` runs `aterm pkg update grok` (`aterm help pkg`) \
                \u{2014} the vendor's own help follows\n";
    let show = ("FAKE_GROK_SHOW", "ATERM_NO_HARNESS");
    let (out, pid) = run(&rig, &["update", "--help"], &[show]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), help);
    assert_eq!(called(), "");
    assert_eq!(rig.starts().len(), 1);
    let start = rig.starts()[0].clone();
    assert_eq!(start.argv, ["update", "--help"], "as typed");
    assert_eq!(
        (start.pid, start.show.as_str()),
        (pid, "ATERM_NO_HARNESS=;")
    );

    // The launcher hands help to its shim, which drops `--no-harness` and exports it.
    let (out, pid) = run(&rig, &["update", "--no-harness", "--help"], &[show]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stderr(&out), help);
    assert_eq!(rig.starts().len(), 2);
    let start = rig.starts()[1].clone();
    assert_eq!(start.argv, ["update", "--help"]);
    assert_eq!(
        (start.pid, start.show.as_str()),
        (pid, "ATERM_NO_HARNESS=1;")
    );

    let (out, _) = run(&rig, &["fix it", "update"], &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(stderr(&out).contains("run `aterm pkg update grok` instead"));
    assert_eq!(called(), "");
    assert_eq!(rig.starts().len(), 2);

    std::fs::remove_file(rig.path_dir.join("aterm")).unwrap();
    let (out, _) = run(&rig, &["update"], &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(stderr(&out).contains("no aterm is on PATH"), "{out:?}");
    assert_eq!(rig.starts().len(), 2, "grok's own updater never runs");

    // No build current yet: aterm's package manager still answers `grok update`.
    let bare = Rig::new("run-update-bare");
    let calls = bare.fake_aterm();
    let (out, _) = run(&bare, &["update"], &[("FAKE_ATERM_EXIT", "1")]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(std::fs::read_to_string(&calls).unwrap_or_default(), UPDATE);
    assert_eq!(
        stderr(&out),
        "atpkg: aterm updates Grok Build from xAI \u{2014} you have (no version recorded); \
         checking now: `aterm pkg update grok`\natpkg: grok (no version recorded) stays; `grok \
         update` tries again\n"
    );
}

/// THE SHIM ENVIRONMENT (aterm `crates/atpkg/src/shim_env.rs`, the `grok` row's `shim_env`,
/// `GROK_DISABLE_AUTOUPDATER=1`, which the launcher's shim exports after the hand-off to the
/// harness): grok runs with the build's `.shim-env` sidecar and the updater switch always off,
/// as the harness's child and in its place; a launch that typed `--no-harness` also gets
/// `ATERM_NO_HARNESS=1`, which the launcher exports for it. A sidecar that breaks atpkg's rule
/// is no environment at all, and the updater stays off.
#[test]
fn run_sets_the_shim_env() {
    let (rig, id) = rig_v1("run-shim");
    let sidecar = rig.program_dir().join(format!("{id}.shim-env"));
    std::fs::write(&sidecar, "GROK_SHIM_PROBE=on\nGROK_DISABLE_AUTOUPDATER=0\n").unwrap();
    let show = ("FAKE_GROK_SHOW", "GROK_SHIM_PROBE ATERM_NO_HARNESS");
    type Case = (
        &'static str,
        &'static [&'static str],
        &'static [(&'static str, &'static str)],
        bool,
        &'static str,
    );
    let cases: [Case; 3] = [
        ("the harness's child", &["fix it"], &[], false, ""),
        (
            "grok in the harness's place",
            &["fix it"],
            &[("ATERM_CHILD", "1"), ("ATERM_NO_HARNESS", "1")],
            true,
            "1",
        ),
        (
            "a launch that typed --no-harness",
            &["--no-harness", "fix it"],
            &[],
            true,
            "1",
        ),
    ];
    for (what, typed, env, in_place, left_out) in cases {
        let before = rig.starts().len();
        let mut all = env.to_vec();
        all.push(show);
        let (out, pid) = run(&rig, typed, &all);
        assert_eq!(out.status.code(), Some(0), "{what}: {out:?}");
        let start = rig.starts()[before].clone();
        assert_eq!(start.autoupdater, "1", "{what}: the updater stays off");
        assert_eq!(
            start.show,
            format!("GROK_SHIM_PROBE=on;ATERM_NO_HARNESS={left_out};"),
            "{what}"
        );
        if in_place {
            assert_eq!((start.pid, start.harness.as_str()), (pid, ""), "{what}");
        } else {
            assert_eq!(start.harness, pid.to_string(), "{what}");
        }
    }
    std::fs::write(&sidecar, "PATH=/nowhere\nGROK_SHIM_PROBE=on\n").unwrap();
    let before = rig.starts().len();
    let (out, _) = run(&rig, &["fix it"], &[show]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let start = rig.starts()[before].clone();
    assert_eq!(start.autoupdater, "1");
    assert_eq!(start.show, "GROK_SHIM_PROBE=;ATERM_NO_HARNESS=;");
}
