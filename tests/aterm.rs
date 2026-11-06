// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness under aterm, end to end: contract 3's start sequence (claim first, then spawn,
//! else exec grok in place), aterm's read of the screen as the safe point, and the crash
//! relaunch, against a fake aterm control socket and fake grok builds.

mod common;

use std::time::Duration;

use common::{
    FakeAterm, GROK_SCREEN, Harness, POLICY_V3, Pty, Rig, exists, ps_state, signal_pid, wait_until,
};

const V1: (u64, u64, u64) = (1, 0, 70);
const V2: (u64, u64, u64) = (1, 0, 71);
const END: Duration = Duration::from_secs(10);
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn exit_code(status: libc::c_int) -> Option<i32> {
    libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
}

fn term_signal(status: libc::c_int) -> Option<i32> {
    libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status))
}

fn rig_v1(name: &str) -> (Rig, u64) {
    let rig = Rig::new(name);
    let id = rig.install(V1);
    rig.make_current(id);
    (rig, id)
}

/// Start the harness on a pty, in a process group of its own (a shell job), in `aterm`'s tab,
/// and wait for the fake grok's first start line.
fn start_in_tab(
    rig: &Rig,
    aterm: Option<&FakeAterm>,
    args: &[&str],
    env: &[(&str, &str)],
) -> (Harness, Pty) {
    let mut pty = Pty::new();
    let mut cmd = rig.command(args);
    if let Some(a) = aterm {
        for (k, v) in a.env() {
            cmd.env(k, v);
        }
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    pty.attach(&mut cmd);
    let h = Harness::spawn_in_own_group(&mut cmd);
    pty.release_slave();
    assert!(
        wait_until(END, || !rig.starts().is_empty()),
        "the fake never started; journal:\n{}",
        rig.journal_text()
    );
    (h, pty)
}

/// End a session whose grok runs: TERM the harness when it supervises, else grok itself.
fn end(h: &mut Harness, rig: &Rig) -> libc::c_int {
    let last = rig.starts().last().cloned().expect("a start");
    if last.pid == h.pid {
        signal_pid(last.pid, libc::SIGTERM);
    } else {
        h.signal(libc::SIGTERM);
    }
    h.wait_end(END)
}

/// THE FAIL-CLOSED RULE (aterm's harness design §3.2, §8.2): under aterm, the harness claims
/// nothing and execs grok in its own place unless aterm serves contract 3, its policy is on
/// for the session, and the claim comes back proven. grok then IS the tab's process: the same
/// pid, no harness pid in its environment, the shim environment applied.
#[test]
fn under_aterm_without_contract_3_or_a_proven_claim_grok_runs_in_the_harness_place() {
    let cases: [(&str, &str, bool, &str); 6] = [
        (
            "an aterm that predates contract 3",
            "OK gen=1 effective=on why=enabled upgrade=true",
            true,
            "contract=absent",
        ),
        (
            "contract 2 only",
            "OK gen=1 effective=on why=enabled contract=2",
            true,
            "contract=2",
        ),
        (
            "the policy off",
            "OK gen=1 effective=off why=autopilot-off contract=2,3",
            true,
            "why=autopilot-off",
        ),
        (
            "no harness policy verb",
            "ERR unknown verb harness",
            true,
            "harness policy answered",
        ),
        ("a refused claim", POLICY_V3, false, "ERR unproven"),
        ("an unconfirmed claim", POLICY_V3, true, "did not confirm"),
    ];
    for (what, policy, admitted, why) in cases {
        let (rig, id) = rig_v1("pass");
        let aterm = FakeAterm::new(&rig, policy);
        {
            let mut st = aterm.state.lock().unwrap();
            if !admitted {
                st.claim_reply = "ERR unproven: not the foreground leader".to_string();
            }
            if why == "did not confirm" {
                st.proven = false;
            }
        }
        let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &[]);
        let start = rig.starts()[0].clone();
        assert_eq!(start.pid, h.pid, "{what}: grok runs in the harness's place");
        assert_eq!(start.build, id);
        assert_eq!(start.harness, "", "{what}: no harness pid");
        assert_eq!(start.depth, "1", "{what}");
        assert_eq!(start.autoupdater, "1", "{what}: the shim environment");
        assert_eq!(
            start.argv,
            ["first prompt"],
            "{what}: the launch's own argv, no injected id"
        );
        let journal = rig.journal_text();
        assert!(
            journal.contains("PASS grok 1.0.70 runs in the harness's place (exec)"),
            "{what}: {journal}"
        );
        assert!(journal.contains(why), "{what}: {journal}");
        assert_eq!(aterm.holder(), None, "{what}: no claim is left standing");
        if why == "did not confirm" {
            assert!(
                aterm
                    .verbs()
                    .iter()
                    .any(|v| v.starts_with("meta unset supervisor holder=grok-alab@")),
                "{what}: an unconfirmed claim is let go: {:?}",
                aterm.verbs()
            );
        }
        assert_eq!(exit_code(end(&mut h, &rig)), Some(0), "{what}");
    }
}

/// The person's `--no-alab-harness` (the twin's `ATERM_NO_HARNESS=1`), a launch that is not a
/// carried interactive session, and a tab whose socket cannot be found all pass through
/// before any word to aterm.
#[test]
fn a_harness_left_out_or_with_nothing_to_carry_passes_through_without_asking_aterm() {
    type Case = (
        &'static str,
        &'static [&'static str],
        &'static [(&'static str, &'static str)],
    );
    let cases: [Case; 4] = [
        ("ATERM_NO_HARNESS=1", &["hi"], &[("ATERM_NO_HARNESS", "1")]),
        ("a subcommand", &["sessions"], &[]),
        ("a resume the harness cannot carry", &["--continue"], &[]),
        (
            "no graph entry",
            &["hi"],
            &[("ATERM_PARENT_SESSION_ID", "s-dead")],
        ),
    ];
    for (what, args, env) in cases {
        let (rig, _) = rig_v1("left-out");
        let aterm = FakeAterm::new(&rig, POLICY_V3);
        let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), args, env);
        let start = rig.starts()[0].clone();
        assert_eq!(start.pid, h.pid, "{what}: exec in place");
        assert_eq!(start.argv, *args, "{what}");
        assert!(
            aterm.verbs().is_empty(),
            "{what}: aterm was asked {:?}",
            aterm.verbs()
        );
        assert!(
            rig.journal_text().contains("PASS grok"),
            "{what}: {}",
            rig.journal_text()
        );
        assert_eq!(exit_code(end(&mut h, &rig)), Some(0), "{what}");
    }
}

/// THE CLAIM: with contract 3 served and the policy on, the harness claims
/// `grok-alab@<its pid> contract=3 scope=relaunch,upgrade` with its own and grok's versions,
/// keeps the claim's connection, and only then runs grok as its child.
#[test]
fn under_contract_3_the_harness_claims_its_scope_first_and_then_runs_grok_as_its_child() {
    let (rig, id) = rig_v1("claim");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &[]);
    let start = rig.starts()[0].clone();
    assert_ne!(start.pid, h.pid, "grok is the harness's child");
    assert_eq!(start.harness, h.pid.to_string());
    assert_eq!(start.build, id);
    assert_eq!(start.argv[0], "--session-id");
    let verbs = aterm.verbs();
    assert_eq!(verbs[0], "harness policy", "{verbs:?}");
    assert_eq!(
        verbs[1],
        format!(
            "meta set supervisor grok-alab@{} contract=3 scope=relaunch,upgrade version={VERSION} vendor=1.0.70",
            h.pid
        )
    );
    assert_eq!(
        verbs[2], "status",
        "the claim is confirmed before grok runs"
    );
    assert_eq!(aterm.holder(), Some(format!("grok-alab@{}", h.pid)));
    let journal = rig.journal_text();
    assert!(
        journal.contains(&format!(
            "CLAIMED grok-alab@{} holds the session under aterm contract 3, scope=relaunch,upgrade",
            h.pid
        )),
        "{journal}"
    );
    // Nothing is waiting, so aterm is not read again: no status poll without a newer build.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(aterm.verbs().len(), 3, "{:?}", aterm.verbs());
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
    assert!(
        wait_until(END, || aterm.holder().is_none()),
        "the claim ends with its connection"
    );
}

/// THE SAFE POINT FROM ATERM, ON A SCREEN ONLY A MEASURED GROK READER GIVES (`agent=idle`,
/// `composer=empty`; no shipping aterm sends it, see the next test): a newer build moves at the
/// first read of aterm that passes, with no signal sent by anyone; a draft, a person typing, an
/// aterm that reports no composer and an approval box each hold it; after the move the claim is
/// re-announced with grok's new version.
#[test]
fn aterms_read_moves_grok_onto_a_newer_build_with_no_signal_and_never_over_a_draft_or_typing() {
    let (rig, id1) = rig_v1("aterm-move");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    aterm.screen(&[("composer", Some("draft"))]);
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&aterm),
        &["--always-approve", "first prompt"],
        &[("GROK_ALAB_QUIET_MS", "0")],
    );
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    let id2 = rig.install(V2);
    rig.make_current(id2);

    let holds: [(&str, Option<&str>, &str); 4] = [
        ("composer", Some("draft"), "composer=draft"),
        (
            "human_ms",
            Some("5000"),
            "a person typed 5000 ms ago (the grace is 120000 ms)",
        ),
        ("agent", Some("prompt"), "aterm reads agent=prompt"),
        ("composer", None, "aterm reports no composer="),
    ];
    for (key, value, why) in holds {
        aterm.screen(&[(key, value)]);
        assert!(
            wait_until(END, || rig.journal_text().contains(why)),
            "{key}={value:?}: no WAIT naming it; journal:\n{}",
            rig.journal_text()
        );
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            rig.starts().len(),
            1,
            "{key}={value:?} held nothing: {}",
            rig.journal_text()
        );
        assert!(h.alive() && exists(first.pid), "{key}={value:?}");
    }
    assert!(rig.journal_text().contains("WAIT not yet a safe point"));

    aterm.screen(&[]);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no move on a quiet read; journal:\n{}",
        rig.journal_text()
    );
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id2);
    assert_eq!(
        second.argv,
        ["--always-approve", "--resume", session.as_str()]
    );
    assert_eq!(
        second.harness,
        h.pid.to_string(),
        "the same harness all along"
    );
    assert!(rig.log_text().contains(&format!("term build={id1}")));
    let journal = rig.journal_text();
    assert!(journal.contains("MOVE safe point: SIGTERM to grok 1.0.70 to relaunch it on 1.0.71 (witness: aterm's read)"), "{journal}");
    assert!(
        wait_until(END, || aterm
            .claims()
            .iter()
            .any(|c| c.ends_with("vendor=1.0.71"))),
        "the claim names the new build: {:?}",
        aterm.claims()
    );
    assert_eq!(aterm.holder(), Some(format!("grok-alab@{}", h.pid)));
    assert!(!exists(first.pid) || ps_state(first.pid).contains('Z'));
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// `[harness] upgrade = false` and `upgrade_ask = true` hold the move; SIGUSR1 stays the
/// person's override.
#[test]
fn the_policy_can_hold_the_move_and_sigusr1_still_overrides() {
    let (rig, _) = rig_v1("aterm-policy");
    let aterm = FakeAterm::new(
        &rig,
        &POLICY_V3.replace("upgrade_ask=false", "upgrade_ask=true"),
    );
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&aterm),
        &["hi"],
        &[
            ("GROK_ALAB_QUIET_MS", "0"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(
        wait_until(END, || rig.journal_text().contains("upgrade_ask = true")),
        "{}",
        rig.journal_text()
    );
    aterm.state.lock().unwrap().policy = POLICY_V3.replace("upgrade=true", "upgrade=false");
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("[harness] upgrade = false")),
        "{}",
        rig.journal_text()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.starts().len(), 1);
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "{}",
        rig.journal_text()
    );
    assert!(rig.journal_text().contains("(witness: SIGUSR1)"));
    assert_eq!(rig.starts()[1].build, id2);
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// THE CRASH RELAUNCH (scope `relaunch`): a grok that dies of a fault is relaunched on the
/// same session; one that dies of a person's KILL, or crashes too soon after its start, is not,
/// and the harness ends as it ended.
#[test]
fn a_crashed_grok_is_relaunched_on_its_session_and_a_killed_or_early_one_is_not() {
    let (rig, id) = rig_v1("crash");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    let fast = [
        ("GROK_ALAB_MIN_RUN_MS", "0"),
        ("GROK_ALAB_CRASH_PAUSE_MS", "0"),
    ];
    let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &fast);
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    signal_pid(first.pid, libc::SIGSEGV);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch; journal:\n{}",
        rig.journal_text()
    );
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id);
    assert_eq!(second.argv, ["--resume", session.as_str()]);
    assert_eq!(second.harness, h.pid.to_string());
    let journal = rig.journal_text();
    assert!(
        journal.contains("CRASH grok 1.0.70 crashed (signal 11)"),
        "{journal}"
    );
    assert!(journal.contains("RELAUNCHED after a crash"), "{journal}");
    signal_pid(second.pid, libc::SIGKILL);
    assert_eq!(
        term_signal(h.wait_end(END)),
        Some(libc::SIGKILL),
        "a KILL is never relaunched"
    );
    assert_eq!(rig.starts().len(), 2);

    let (rig, _) = rig_v1("crash-early");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &[]);
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1);
    assert!(
        rig.journal_text()
            .contains("a refused start is never looped on"),
        "{}",
        rig.journal_text()
    );

    // Outside aterm nothing claims `relaunch`, so a crash ends the session.
    let (rig, _) = rig_v1("crash-alone");
    let (mut h, _pty) = start_in_tab(&rig, None, &["first prompt"], &fast);
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert!(
        rig.journal_text().contains("no claim with relaunch scope"),
        "{}",
        rig.journal_text()
    );
}

/// Lays a fake `aterm` executable on the rig's PATH that records each call and exits
/// `$FAKE_ATERM_EXIT` (default 0); returns the calls file. (The claim goes to the fake control
/// socket; this is the binary the sign-in gate runs.)
fn fake_aterm_bin(rig: &Rig) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let calls = rig.root.join("aterm.calls");
    let aterm = rig.path_dir.join("aterm");
    std::fs::write(
        &aterm,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit \"${{FAKE_ATERM_EXIT:-0}}\"\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).unwrap();
    calls
}

/// The rig's grok home login (`$HOME/.grok/auth.json`).
fn login(rig: &Rig) -> std::path::PathBuf {
    rig.home.join(".grok").join("auth.json")
}

/// A machine signed out mid-session (`grok logout` in another tab, or a login aterm saw grok
/// refuse) is signed in again before a crashed grok is relaunched: aterm's sign-in runs first,
/// and a refused one ends the session as a signed-out relaunch does (the crash's own status and
/// the resume hint) instead of a signed-out grok that opens its own page. CONTROL: aterm signs
/// it in, and the relaunch follows; that case signs out by aterm's refusal marker alone.
#[test]
fn a_crash_relaunch_on_a_signed_out_machine_asks_aterm_to_sign_in_first() {
    let signin = "harness signin --cli grok --wait\n";
    for (exit, relaunched) in [("1", false), ("0", true)] {
        let (rig, _) = rig_v1(&format!("crash-signin-{exit}"));
        let aterm = FakeAterm::new(&rig, POLICY_V3);
        let calls = fake_aterm_bin(&rig);
        std::fs::create_dir_all(login(&rig).parent().unwrap()).unwrap();
        std::fs::write(login(&rig), "{}").unwrap();
        let env = [
            ("ATERM_CHILD", "1"),
            ("FAKE_ATERM_EXIT", exit),
            ("GROK_ALAB_MIN_RUN_MS", "0"),
            ("GROK_ALAB_CRASH_PAUSE_MS", "0"),
        ];
        let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &env);
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap_or_default(),
            "",
            "signed in at the start: aterm is not asked"
        );
        let first = rig.starts()[0].clone();
        assert_ne!(first.pid, h.pid, "grok is the harness's child");
        if relaunched {
            // aterm saw grok refuse this login: its marker, newer than the login, signs out.
            let marker = login(&rig).with_extension("json.refused");
            std::fs::write(&marker, "").unwrap();
            let marked = std::fs::metadata(&marker).unwrap().modified().unwrap();
            std::fs::File::options()
                .write(true)
                .open(login(&rig))
                .unwrap()
                .set_modified(marked - Duration::from_secs(5))
                .unwrap();
        } else {
            std::fs::remove_file(login(&rig)).unwrap();
        }
        signal_pid(first.pid, libc::SIGSEGV);
        if relaunched {
            assert!(
                wait_until(END, || rig.starts().len() == 2),
                "CONTROL: no relaunch; journal:\n{}",
                rig.journal_text()
            );
            assert_eq!(std::fs::read_to_string(&calls).unwrap(), signin);
            assert!(rig.journal_text().contains("RELAUNCHED after a crash"));
            assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
        } else {
            assert_eq!(
                term_signal(h.wait_end(END)),
                Some(libc::SIGSEGV),
                "journal:\n{}",
                rig.journal_text()
            );
            assert_eq!(rig.starts().len(), 1, "a signed-out grok was relaunched");
            assert_eq!(std::fs::read_to_string(&calls).unwrap(), signin);
            let journal = rig.journal_text();
            assert!(
                journal.contains(
                    "REFUSED the sign-in before the relaunch: not signed in to Grok Build"
                ),
                "{journal}"
            );
            assert!(journal.contains("would start signed out"), "{journal}");
        }
    }
}

/// A newer build waits while the machine is signed out (the relaunch would have to sign in),
/// and moves once a login is back; aterm's sign-in is never asked for while the child runs, so
/// a working session is never ended for a move that would stop at the sign-in.
#[test]
fn a_move_waits_while_the_machine_is_signed_out() {
    let (rig, _) = rig_v1("move-signin");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    // A screen only a measured Grok reader gives, so only the sign-in holds the move.
    aterm.screen(&[]);
    let calls = fake_aterm_bin(&rig);
    std::fs::create_dir_all(login(&rig).parent().unwrap()).unwrap();
    std::fs::write(login(&rig), "{}").unwrap();
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&aterm),
        &["first prompt"],
        &[
            ("ATERM_CHILD", "1"),
            ("FAKE_ATERM_EXIT", "1"),
            ("GROK_ALAB_QUIET_MS", "0"),
        ],
    );
    std::fs::remove_file(login(&rig)).unwrap();
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("would start signed out")),
        "no WAIT naming the sign-in; journal:\n{}",
        rig.journal_text()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.starts().len(), 1, "moved while signed out");
    assert!(h.alive());
    std::fs::write(login(&rig), "{}").unwrap();
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no move once signed in; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(rig.starts()[1].build, id2);
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap_or_default(),
        "",
        "aterm was asked while the child ran"
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// `grok-alab run -- <argv>` (what aterm's `grok` twin runs) takes every word after `--` as
/// grok's, `--version` included.
#[test]
fn run_hands_every_word_after_it_to_grok() {
    let (rig, _) = rig_v1("run");
    let (mut h, _pty) = start_in_tab(
        &rig,
        None,
        &["run", "--", "--always-approve", "first prompt"],
        &[],
    );
    let start = rig.starts()[0].clone();
    assert_eq!(start.argv[0], "--session-id");
    assert_eq!(&start.argv[2..], ["--always-approve", "first prompt"]);
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));

    let (rig, _) = rig_v1("run-version");
    let (mut h, _pty) = start_in_tab(&rig, None, &["run", "--", "--version"], &[]);
    assert_eq!(
        rig.starts()[0].argv,
        ["--version"],
        "grok's own --version, not the harness's"
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// ON THE ATERM THAT SHIPS, aterm's read never moves grok: aterm reports `agent=unknown` for
/// every Grok session (its Grok reader has no authoritative phase) and no `composer=`, so the
/// automatic move holds, named in the journal, and `SIGUSR1` is still what moves it. This is the
/// screen [`common::FakeAterm`] starts from, so no test passes on a screen aterm never sends.
#[test]
fn on_the_aterm_that_ships_no_move_is_taken_on_its_read_and_sigusr1_still_moves() {
    let (rig, _) = rig_v1("aterm-today");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    assert!(GROK_SCREEN.contains(&("agent", "unknown")));
    assert!(!GROK_SCREEN.iter().any(|(k, _)| *k == "composer"));
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&aterm),
        &["hi"],
        &[
            ("GROK_ALAB_QUIET_MS", "0"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("WAIT not yet a safe point: aterm reads agent=unknown")),
        "{}",
        rig.journal_text()
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "{}",
        rig.journal_text()
    );
    assert!(rig.journal_text().contains("(witness: SIGUSR1)"));
    assert_eq!(rig.starts()[1].build, id2);
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A CRASH IS RELAUNCHED ONLY ON A FRESH READ: a person who turns `[harness] relaunch` off
/// mid-session gets no relaunch, though the claim was made under `relaunch=true` and no event
/// said so (the read before the act sees it).
#[test]
fn a_crash_is_relaunched_only_when_a_fresh_read_still_allows_it() {
    let (rig, _) = rig_v1("crash-fresh");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    let fast = [
        ("GROK_ALAB_MIN_RUN_MS", "0"),
        ("GROK_ALAB_CRASH_PAUSE_MS", "0"),
    ];
    let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &fast);
    aterm.state.lock().unwrap().policy = POLICY_V3.replace("relaunch=true", "relaunch=false");
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(
        rig.journal_text()
            .contains("not relaunched: [harness] relaunch is off"),
        "{}",
        rig.journal_text()
    );
}

/// AUTOPILOT OFF GIVES THE CLAIM BACK: on aterm's `harness-policy` event with the policy off, the
/// harness unsets its claim and acts on nothing; back on, it claims again. A policy turned off
/// with no event is still caught by the fresh read before a crash relaunch, which gives the
/// claim back too.
#[test]
fn under_autopilot_off_the_claim_is_given_back_and_a_crash_is_not_relaunched() {
    let (rig, _) = rig_v1("autopilot-off");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    let fast = [
        ("GROK_ALAB_MIN_RUN_MS", "0"),
        ("GROK_ALAB_CRASH_PAUSE_MS", "0"),
    ];
    let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &fast);
    let holder = format!("grok-alab@{}", h.pid);
    assert!(
        wait_until(END, || aterm.subscriptions() == 1),
        "the harness subscribes to the session's events"
    );
    // An event the harness reads past.
    aterm.event("EVENT 1 status program_state=-");
    let off = "OK gen=2 effective=off why=autopilot-off contract=2,3 relaunch=true upgrade=true \
               upgrade_ask=false";
    aterm.policy_event(off);
    assert!(
        wait_until(END, || aterm.holder().is_none()),
        "the claim stands under Autopilot Off; journal:\n{}",
        rig.journal_text()
    );
    assert!(
        aterm
            .verbs()
            .iter()
            .any(|v| v == &format!("meta unset supervisor holder={holder}")),
        "{:?}",
        aterm.verbs()
    );
    assert!(
        rig.journal_text().contains("RELEASED grok-alab@")
            && rig.journal_text().contains("why=autopilot-off"),
        "{}",
        rig.journal_text()
    );
    // Back on: claimed again.
    aterm.policy_event(POLICY_V3);
    assert!(
        wait_until(END, || aterm.holder() == Some(holder.clone())),
        "not claimed again; journal:\n{}",
        rig.journal_text()
    );
    assert!(rig.journal_text().contains("harness policy is back on"));
    // Off again with no event: the crash relaunch's fresh read sees it.
    aterm.state.lock().unwrap().policy = off.to_string();
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(
        rig.journal_text().contains(
            "not relaunched: aterm's harness policy is off for this session (why=autopilot-off)"
        ),
        "{}",
        rig.journal_text()
    );
}

/// AN ATERM RESTART DROPS THE CLAIM, AND THE HARNESS MAKES IT AGAIN AT ONCE: it notices the
/// closed connection on its next tick, with no newer build waiting and no act to take, rather
/// than at its next act (aterm's re-proof window after a seamless update). Under an aterm that
/// gives no event stream (the policy then followed on a 5 s clock) the closed claim connection
/// alone is noticed, well inside that clock.
#[test]
fn a_claim_aterm_dropped_is_made_again_without_waiting_for_an_act() {
    for subscribed in [true, false] {
        let (rig, _) = rig_v1("aterm-restart");
        let aterm = FakeAterm::new(&rig, POLICY_V3);
        aterm.state.lock().unwrap().refuse_subscribe = !subscribed;
        let (mut h, _pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &[]);
        let holder = format!("grok-alab@{}", h.pid);
        assert_eq!(aterm.holder(), Some(holder.clone()));
        assert!(
            rig.journal_text().contains(if subscribed {
                "policy followed on its harness-policy event"
            } else {
                "policy followed on a clock: aterm gave no event stream"
            }),
            "subscribed={subscribed}: {}",
            rig.journal_text()
        );
        let claims = aterm.claims().len();
        aterm.restart();
        assert_eq!(aterm.holder(), None);
        assert!(
            wait_until(Duration::from_secs(3), || aterm.holder()
                == Some(holder.clone())),
            "subscribed={subscribed}: the claim was not made again; journal:\n{}",
            rig.journal_text()
        );
        assert_eq!(aterm.claims().len(), claims + 1);
        // The journal line follows aterm's confirmation.
        assert!(
            wait_until(END, || rig
                .journal_text()
                .contains("holds the session again")),
            "{}",
            rig.journal_text()
        );
        assert!(
            rig.journal_text()
                .contains("LOST the claim's connection to aterm failed"),
            "{}",
            rig.journal_text()
        );
        assert_eq!(rig.starts().len(), 1, "grok was never touched");
        assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
    }
}

/// A SIGNED-OUT GROK IS NEVER RELAUNCHED: a relaunch is the harness's own act, and a signed-out
/// Grok Build opens xAI's sign-in page in the desk's browser. The crash ends the session, and
/// the person is told why and how to resume.
#[test]
fn a_crash_while_grok_is_signed_out_is_not_relaunched() {
    let (rig, _) = rig_v1("crash-signed-out");
    let aterm = FakeAterm::new(&rig, POLICY_V3);
    let fast = [
        ("GROK_ALAB_MIN_RUN_MS", "0"),
        ("GROK_ALAB_CRASH_PAUSE_MS", "0"),
    ];
    let (mut h, pty) = start_in_tab(&rig, Some(&aterm), &["first prompt"], &fast);
    let session = rig.starts()[0].argv[1].clone();
    rig.sign_out();
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(
        rig.journal_text().contains("would start signed out"),
        "{}",
        rig.journal_text()
    );
    let hint = format!("grok --resume {session}");
    assert!(
        wait_until(END, || String::from_utf8_lossy(&pty.output.lock().unwrap())
            .contains(&hint)),
        "{:?}",
        String::from_utf8_lossy(&pty.output.lock().unwrap())
    );
}
