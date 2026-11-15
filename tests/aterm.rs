// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness under aterm, end to end, against `alab-wire`'s fake aterm control socket
//! (`alab_wire::fake`) and fake grok builds: the launch's claim (at contract 4, or 3 behind the
//! roster guard), grok run in the harness's place when aterm refuses it, supervised and claimed
//! again when aterm cannot take it now; aterm's read of the screen as the safe point; the crash
//! relaunch on aterm's word and back-off; and the restarts aterm asks for.

mod common;

use std::time::Duration;

use alab_wire::Scope;
use alab_wire::fake::{FakeAterm, POLICY_ON};
use common::{END, Harness, Pty, Rig, exists, ps_state, signal_pid, ticks, wait_until, wait_up};

const V1: (u64, u64, u64) = (1, 0, 70);
const V2: (u64, u64, u64) = (1, 0, 71);
const VERSION: &str = grok_alab::VERSION;

/// What aterm answers while it shows a macOS dialog (`control_media.rs`
/// `dialog_refusal_for`): a refusal that asks to be retried.
const DIALOG: &str = "ERR aterm is showing a dialog; answer it, then retry";

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

/// The policy body [`POLICY_ON`] with one word replaced.
fn policy(from: &str, to: &str) -> String {
    assert!(POLICY_ON.contains(from), "{from}");
    POLICY_ON.replace(from, to)
}

/// WHAT ATERM REPORTS FOR A GROK SESSION TODAY (aterm main, 2026-10-25): its Grok reader has no
/// authoritative phase (`GrokReader::phase` is always a non-authoritative idle,
/// `crates/aterm-phase/src/reader.rs`), which presence turns into `agent=unknown`
/// (`crates/aterm-gui/src/presence.rs`), and `status` carries no `composer=`. Nobody typing or
/// holding the session. Every fake starts from this screen, so a move on aterm's read is never
/// tested against a screen aterm does not send.
fn aterm(name: &str) -> FakeAterm {
    let fake = FakeAterm::for_harness(name, &grok_alab::aterm::config());
    fake.set_status("agent", Some("unknown"));
    fake.set_status("composer", None);
    fake
}

/// A quiet screen as an aterm WITH A MEASURED GROK READER would report it (not shipped): the
/// turn over (`agent=idle`), the composer empty, nobody typing or holding the session.
fn reader_screen(fake: &FakeAterm) {
    fake.set_status("agent", Some("idle"));
    fake.set_status("composer", Some("empty"));
}

/// The claims sent so far (`meta set supervisor …`).
fn claims(fake: &FakeAterm) -> Vec<String> {
    fake.verbs()
        .into_iter()
        .filter(|v| v.starts_with("meta set supervisor "))
        .collect()
}

fn released(fake: &FakeAterm, holder: &str) -> bool {
    fake.verbs()
        .iter()
        .any(|v| v == &format!("meta unset supervisor holder={holder}"))
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
    // Its first output written, so nothing it says later is its start.
    wait_up(&pty, rig.starts()[0].pid);
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

/// Wait for the journal to say `what`.
fn journal_says(rig: &Rig, what: &str) {
    assert!(
        wait_until(END, || rig.journal_text().contains(what)),
        "the journal never said {what:?}:\n{}",
        rig.journal_text()
    );
}

/// THE FAIL-CLOSED RULE (aterm's harness design §3.2, §8.2): under aterm, a claim aterm refuses
/// leaves grok in the harness's own place. grok then IS the tab's process: the same pid, no
/// harness pid in its environment, the shim environment applied. That is a refused or unproven
/// claim, a policy off by a fact of the window, an `ERR` that does not ask to be retried (an
/// aterm without the verb), a policy with no `contract=` at all (an aterm from before contract
/// 3, which never claims at one), a refused event subscription, and a grant short of both
/// duties (given back first).
#[test]
fn under_aterm_a_refused_claim_runs_grok_in_the_harness_place() {
    type Setup = fn(&FakeAterm);
    let cases: [(&str, Setup, &str); 7] = [
        (
            "a refused claim",
            |f| f.set_claim_reply("ERR unproven: not the foreground leader"),
            "claim reply=ERR:unproven:",
        ),
        (
            "an unconfirmed claim",
            |f| f.set_proven(false),
            "unproven supervisor=grok-alab@",
        ),
        (
            "a headless window",
            |f| {
                f.set_policy(&policy(
                    "effective=on why=enabled",
                    "effective=off why=headless",
                ))
            },
            "policy effective=off why=headless",
        ),
        (
            "an aterm without the verb",
            |f| f.set_policy_reply("ERR unknown verb harness"),
            "policy reply=ERR:unknown",
        ),
        (
            "an aterm that predates contract 3",
            |f| {
                f.set_policy(
                    "effective=on why=enabled relaunch=true upgrade=true upgrade_ask=false",
                )
            },
            "policy contract=absent",
        ),
        (
            "a refused subscription",
            |f| f.set_subscribe_reply(Some("ERR usage: subscribe")),
            "events",
        ),
        (
            "a grant without upgrade",
            |f| f.set_cap(Some(&[Scope::Relaunch])),
            "aterm granted scope=relaunch; the claim needs relaunch,upgrade",
        ),
    ];
    for (what, setup, why) in cases {
        let (rig, id) = rig_v1("pass");
        let fake = aterm("pass");
        setup(&fake);
        let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
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
        assert_eq!(fake.holder(), None, "{what}: no claim is left standing");
        if what.contains("unconfirmed") || what.contains("grant") {
            assert!(
                fake.verbs()
                    .iter()
                    .any(|v| v.starts_with("meta unset supervisor holder=grok-alab@")),
                "{what}: a claim taken is given back: {:?}",
                fake.verbs()
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
        let fake = aterm("left-out");
        let (mut h, _pty) = start_in_tab(&rig, Some(&fake), args, env);
        let start = rig.starts()[0].clone();
        assert_eq!(start.pid, h.pid, "{what}: exec in place");
        assert_eq!(start.argv, *args, "{what}");
        assert!(
            fake.verbs().is_empty(),
            "{what}: aterm was asked {:?}",
            fake.verbs()
        );
        assert!(
            rig.journal_text().contains("PASS grok"),
            "{what}: {}",
            rig.journal_text()
        );
        assert_eq!(exit_code(end(&mut h, &rig)), Some(0), "{what}");
    }
}

/// THE CLAIM AT CONTRACT 4: the harness claims `grok-alab@<its pid> contract=4
/// scope=relaunch,upgrade` with its own and grok's versions and no roster read, reads its proof
/// back, and only then runs grok as its child, which it names to aterm at once
/// (`child=<pid>:<start>`). With nothing waiting it asks aterm nothing more.
#[test]
fn the_claim_is_made_at_contract_4_and_names_the_child_grok_runs_as() {
    let (rig, id) = rig_v1("claim");
    let fake = aterm("claim");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let start = rig.starts()[0].clone();
    assert_ne!(start.pid, h.pid, "grok is the harness's child");
    assert_eq!(start.harness, h.pid.to_string());
    assert_eq!(start.build, id);
    assert_eq!(start.argv[0], "--session-id");
    let holder = format!("grok-alab@{}", h.pid);
    let claim = format!(
        "meta set supervisor {holder} contract=4 scope=relaunch,upgrade version={VERSION} vendor=1.0.70"
    );
    assert!(
        wait_until(END, || fake.verbs().len() >= 4),
        "{:?}",
        fake.verbs()
    );
    let verbs = fake.verbs();
    assert_eq!(verbs[0], "harness policy", "{verbs:?}");
    assert_eq!(verbs[1], claim, "no roster read under contract 4");
    assert_eq!(verbs[2], "status", "the claim is proven before grok runs");
    assert!(
        verbs[3].starts_with(&format!("{claim} child={}:", start.pid)),
        "the child is named once it runs: {verbs:?}"
    );
    assert!(
        fake.child()
            .is_some_and(|c| c.starts_with(&format!("{}:", start.pid))),
        "{:?}",
        fake.child()
    );
    assert_eq!(fake.holder(), Some(holder.clone()));
    let journal = rig.journal_text();
    assert!(
        journal.contains(&format!(
            "CLAIMED holder={holder} contract=4 scope=relaunch,upgrade revision=1 vendor=1.0.70"
        )),
        "{journal}"
    );
    assert!(
        journal.contains(&format!(
            "{holder} holds the session under aterm contract 4, scope=relaunch,upgrade"
        )) && journal.contains("policy followed on its harness-policy event"),
        "{journal}"
    );
    assert!(
        journal.contains(&format!("REANNOUNCED vendor=1.0.70 child={}:", start.pid)),
        "{journal}"
    );
    // Nothing is waiting, so aterm is not read again: no status poll without a newer build.
    // Watched for eight ticks, each of which would read aterm if a tick read it unasked.
    std::thread::sleep(ticks(8));
    assert_eq!(fake.verbs().len(), 4, "{:?}", fake.verbs());
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
    assert!(
        wait_until(END, || fake.holder().is_none()),
        "the claim ends with its connection"
    );
}

/// AGAINST AN ATERM WITHOUT CONTRACT 4 the claim is contract 3's: the roster is read first
/// (`native-act info` must list `grok-alab:upgrade`), and the child is announced again with
/// the same line, which contract 3 gives no `child=`.
#[test]
fn against_an_aterm_without_contract_4_the_claim_is_contract_3s_behind_the_roster_guard() {
    let (rig, _) = rig_v1("claim-3");
    let fake = aterm("claim-3");
    fake.serve_contracts(&[2, 3]);
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    let claim = format!(
        "meta set supervisor {holder} contract=3 scope=relaunch,upgrade version={VERSION} vendor=1.0.70"
    );
    assert!(
        wait_until(END, || fake.verbs().len() >= 5),
        "{:?}",
        fake.verbs()
    );
    assert_eq!(
        fake.verbs()[..5],
        [
            "harness policy",
            "native-act info",
            &claim,
            "status",
            &claim
        ],
        "the roster before the claim, and no child= under contract 3"
    );
    assert_eq!(fake.holder(), Some(holder.clone()));
    assert_eq!(fake.child(), None);
    assert!(
        rig.journal_text()
            .contains(&format!("CLAIMED holder={holder} contract=3")),
        "{}",
        rig.journal_text()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// THE ROSTER GUARD AND THE SCOPE READ-BACK under contract 3 (Decision 37): an aterm whose
/// `native-act info` lists no `grok-alab:upgrade` row is never sent a claim, not even to find
/// out (aterm stores an unrostered contract-3 claim as a foreign one that takes every duty),
/// and a claim aterm stores without both duties in `supervisor_scope=` is let go. Both pass
/// through.
#[test]
fn an_unrostered_harness_claims_nothing_and_a_short_scope_is_let_go() {
    type Setup = fn(&FakeAterm);
    let cases: [(&str, Setup, &str, bool); 3] = [
        (
            "no grok-alab row",
            |f| f.set_row("grok-alab", &[]),
            "unrostered",
            false,
        ),
        (
            "an aterm without the verb",
            |f| f.set_info("ERR unknown verb native-act"),
            "unrostered",
            false,
        ),
        (
            "relaunch only",
            |f| f.set_cap_after_next_claim(Some(&[Scope::Relaunch])),
            "unproven scope=upgrade supervisor_scope=relaunch",
            true,
        ),
    ];
    for (what, setup, why, claimed) in cases {
        let (rig, _) = rig_v1("roster");
        let fake = aterm("roster");
        fake.serve_contracts(&[2, 3]);
        setup(&fake);
        let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
        assert_eq!(
            rig.starts()[0].pid,
            h.pid,
            "{what}: grok runs in the harness's place"
        );
        let journal = rig.journal_text();
        assert!(journal.contains("PASS grok"), "{what}: {journal}");
        assert!(journal.contains(why), "{what}: {journal}");
        assert_eq!(fake.holder(), None, "{what}: no claim is left standing");
        assert_eq!(
            claims(&fake).is_empty(),
            !claimed,
            "{what}: {:?}",
            fake.verbs()
        );
        if claimed {
            assert!(
                fake.verbs()
                    .iter()
                    .any(|v| v.starts_with("meta unset supervisor holder=grok-alab@")),
                "{what}: {:?}",
                fake.verbs()
            );
        }
        assert_eq!(exit_code(end(&mut h, &rig)), Some(0), "{what}");
    }
}

/// One case of [`claimed_once_aterm_can`]: what it is, how the fake starts, how it comes to
/// answer, and the launch's `UNCLAIMED` words.
type Later = (&'static str, fn(&FakeAterm), fn(&FakeAterm), &'static str);

/// An aterm that cannot take the claim now keeps grok the harness's child, unclaimed, and the
/// claim is made every 5 s until it stands (Decisions 35, 40, 45): aterm busy (a dialog's `ERR
/// … retry` to the policy or to the proof, a launch read answered late), an aterm that serves
/// no contract this harness speaks yet, and a policy a person switched off.
fn claimed_once_aterm_can(cases: &[Later]) {
    for (what, setup, answer, why) in cases {
        let (rig, _) = rig_v1("later");
        let fake = aterm("later");
        setup(&fake);
        let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
        let start = rig.starts()[0].clone();
        assert_ne!(start.pid, h.pid, "{what}: grok is the harness's child");
        assert_eq!(start.harness, h.pid.to_string(), "{what}");
        let journal = rig.journal_text();
        assert!(
            journal.contains(&format!("UNCLAIMED {why}")),
            "{what}: {journal}"
        );
        assert!(
            journal.contains("grok runs as the harness's child, unclaimed"),
            "{what}: {journal}"
        );
        assert!(!journal.contains("PASS grok"), "{what}: {journal}");
        assert_eq!(fake.holder(), None, "{what}: no claim stands");
        answer(&fake);
        let holder = format!("grok-alab@{}", h.pid);
        assert!(
            wait_until(END, || fake.holder() == Some(holder.clone())),
            "{what}: never claimed once aterm could; journal:\n{}",
            rig.journal_text()
        );
        journal_says(&rig, &format!("CLAIMED holder={holder} contract=4"));
        assert!(h.alive() && exists(start.pid), "{what}");
        assert_eq!(exit_code(end(&mut h, &rig)), Some(0), "{what}");
    }
}

#[test]
fn a_busy_aterm_at_the_launch_keeps_grok_supervised_and_claims_once_it_answers() {
    claimed_once_aterm_can(&[
        (
            "a dialog up at the policy",
            |f| f.set_policy_reply(DIALOG),
            |f| f.clear_policy_reply(),
            "policy",
        ),
        (
            "a dialog up at the proof",
            |f| f.set_status_reply(DIALOG),
            |f| f.clear_status_reply(),
            "status",
        ),
        (
            "a launch read answered late",
            |f| f.set_reply_delay(Duration::from_millis(900)),
            |f| f.set_reply_delay(Duration::ZERO),
            "policy",
        ),
    ]);
}

#[test]
fn an_aterm_that_serves_no_contract_yet_or_is_switched_off_is_claimed_once_it_admits_one() {
    claimed_once_aterm_can(&[
        (
            "contract 2 only",
            |f| f.serve_contracts(&[2]),
            |f| f.serve_contracts(&[2, 3, 4]),
            "policy contract=2",
        ),
        (
            "Autopilot Off",
            |f| {
                f.set_policy(&policy(
                    "effective=on why=enabled",
                    "effective=off why=autopilot-off",
                ));
            },
            |f| f.set_policy(POLICY_ON),
            "policy effective=off why=autopilot-off",
        ),
    ]);
}

/// THE SAFE POINT FROM ATERM, ON A SCREEN ONLY A MEASURED GROK READER GIVES (`agent=idle`,
/// `composer=empty`; no shipping aterm sends it, see the next test): a newer build moves at the
/// first read of aterm that passes, with no signal sent by anyone; a draft, a person typing, an
/// approval box and an aterm that reports no composer each hold it; after the move the new
/// child is announced with grok's new version.
#[test]
fn aterms_read_moves_grok_onto_a_newer_build_with_no_signal_and_never_over_a_draft_or_typing() {
    let (rig, id1) = rig_v1("aterm-move");
    let fake = aterm("aterm-move");
    reader_screen(&fake);
    fake.set_status("composer", Some("draft"));
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["--always-approve", "first prompt"],
        &[("GROK_ALAB_QUIET_MS", "0")],
    );
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    let id2 = rig.install(V2);
    rig.make_current(id2);

    let holds: [(&str, Option<&str>, &str); 4] = [
        ("composer", Some("draft"), "wait aterm composer=draft"),
        ("human_ms", Some("5000"), "wait aterm human_ms=5000"),
        ("agent", Some("prompt"), "wait aterm agent=prompt"),
        ("composer", None, "wait aterm composer=absent"),
    ];
    for (key, value, why) in holds {
        reader_screen(&fake);
        fake.set_status("human_ms", Some("-"));
        fake.set_status(key, value);
        journal_says(&rig, why);
        // The WAIT is one tick's answer; aterm is read again on every tick after it, so six
        // more ticks are watched for a move.
        std::thread::sleep(ticks(6));
        assert_eq!(
            rig.starts().len(),
            1,
            "{key}={value:?} held nothing: {}",
            rig.journal_text()
        );
        assert!(h.alive() && exists(first.pid), "{key}={value:?}");
    }
    assert!(rig.journal_text().contains("WAIT not yet a safe point"));

    reader_screen(&fake);
    fake.set_status("human_ms", Some("-"));
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
        wait_until(END, || fake
            .child()
            .is_some_and(|c| c.starts_with(&format!("{}:", second.pid)))
            && claims(&fake)
                .last()
                .is_some_and(|c| c.contains(" vendor=1.0.71 "))),
        "the new child and build are announced: {:?}",
        claims(&fake)
    );
    assert_eq!(fake.holder(), Some(format!("grok-alab@{}", h.pid)));
    assert!(!exists(first.pid) || ps_state(first.pid).contains('Z'));
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// `[harness] upgrade = false` and `upgrade_ask = true` hold the move; SIGUSR1 stays the
/// person's override.
#[test]
fn the_policy_can_hold_the_move_and_sigusr1_still_overrides() {
    let (rig, _) = rig_v1("aterm-policy");
    let fake = aterm("aterm-policy");
    reader_screen(&fake);
    fake.set_policy(&policy("upgrade_ask=false", "upgrade_ask=true"));
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["hi"],
        &[
            ("GROK_ALAB_QUIET_MS", "0"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    journal_says(&rig, "wait aterm upgrade_ask=true");
    fake.set_policy_quietly(&policy("upgrade=true", "upgrade=false"));
    journal_says(&rig, "wait aterm upgrade=false");
    // Six more ticks, each reading aterm again, are watched for a move.
    std::thread::sleep(ticks(6));
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

/// UPGRADE ONLY WHEN GRANTED: a halt that comes over a standing claim (aterm's `hold=1`, which
/// its policy reads as `effective=off why=hold`, with no event) holds aterm's read and a
/// person's `SIGUSR1` alike, the claim kept; and a person's cap that takes `upgrade` from the
/// standing claim hands the move back to aterm's own engine: aterm's read holds on the claim's
/// scope, and a `SIGUSR1` is refused on it too.
#[test]
fn a_halt_or_a_cap_without_upgrade_holds_every_move() {
    let (rig, _) = rig_v1("aterm-cap");
    let fake = aterm("aterm-cap");
    reader_screen(&fake);
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["hi"],
        &[
            ("GROK_ALAB_QUIET_MS", "0"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
        ],
    );
    let holder = format!("grok-alab@{}", h.pid);
    assert_eq!(fake.holder(), Some(holder.clone()));
    fake.set_status("hold", Some("1"));
    let id2 = rig.install(V2);
    rig.make_current(id2);
    let halt = "wait aterm policy effective=off why=hold";
    journal_says(&rig, &format!("WAIT not yet a safe point: {halt}"));
    h.signal(libc::SIGUSR1);
    journal_says(&rig, &format!("HOLD safe point refused: {halt}"));
    assert_eq!(fake.holder(), Some(holder), "a halt keeps the claim");
    fake.set_status("hold", Some("0"));
    fake.set_cap(Some(&[Scope::Relaunch]));
    journal_says(&rig, "NARROWED scope=relaunch");
    journal_says(
        &rig,
        "WAIT not yet a safe point: wait aterm supervisor_scope=relaunch",
    );
    h.signal(libc::SIGUSR1);
    journal_says(
        &rig,
        "HOLD safe point refused: aterm supervisor_scope=relaunch",
    );
    std::thread::sleep(ticks(6));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// ON THE ATERM THAT SHIPS, aterm's read never moves grok: aterm reports `agent=unknown` for
/// every Grok session (its Grok reader has no authoritative phase) and no `composer=`, so the
/// automatic move holds, named in the journal, and `SIGUSR1` is still what moves it. This is the
/// screen [`aterm`] starts from, so no test passes on a screen aterm never sends.
#[test]
fn on_the_aterm_that_ships_no_move_is_taken_on_its_read_and_sigusr1_still_moves() {
    let (rig, _) = rig_v1("aterm-today");
    let fake = aterm("aterm-today");
    assert!(fake.status_line().contains(" agent=unknown"));
    assert!(!fake.status_line().contains(" composer="));
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["hi"],
        &[
            ("GROK_ALAB_QUIET_MS", "0"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    journal_says(&rig, "WAIT not yet a safe point: wait aterm agent=unknown");
    // Ten more ticks, each reading aterm again, are watched for a move.
    std::thread::sleep(ticks(10));
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

/// THE CRASH RELAUNCH, ON ATERM'S LADDER: a grok that dies of a fault is relaunched on the same
/// session after aterm's first pause (1 s), on a fresh read that permits it, and named to aterm;
/// a second crash waits aterm's second pause (10 s), and a `TERM` to the harness while it waits
/// ends the session with the crash's own status, never a relaunch.
#[test]
fn a_crashed_grok_is_relaunched_on_aterms_ladder_and_a_term_in_its_pause_ends_it() {
    let (rig, id) = rig_v1("crash");
    let fake = aterm("crash");
    let (mut h, pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
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
        journal.contains("CRASH grok 1.0.70 crashed (signal 11) after ")
            && journal.contains(&format!(
                "relaunch 1 on session {session} in 1000 ms (aterm's back-off)"
            )),
        "{journal}"
    );
    assert!(journal.contains("RELAUNCHED after a crash"), "{journal}");
    assert!(
        wait_until(END, || fake
            .child()
            .is_some_and(|c| c.starts_with(&format!("{}:", second.pid)))),
        "the relaunched child is named: {:?}",
        fake.child()
    );
    wait_up(&pty, second.pid);
    signal_pid(second.pid, libc::SIGSEGV);
    journal_says(
        &rig,
        &format!("relaunch 2 on session {session} in 10000 ms"),
    );
    h.signal(libc::SIGTERM);
    assert_eq!(
        term_signal(h.wait_end(END)),
        Some(libc::SIGSEGV),
        "the crash's own end: {}",
        rig.journal_text()
    );
    assert_eq!(rig.starts().len(), 2, "{}", rig.journal_text());
    assert!(
        rig.journal_text()
            .contains("not relaunched: signal 15 came while it waited"),
        "{}",
        rig.journal_text()
    );
}

/// A crash is the duty of a claim: outside aterm nothing claims `relaunch`, so a crash ends the
/// session; and aterm's rule leaves a crash with a person at the keys (`human_ms` inside the
/// grace) to the person, so it is not relaunched either.
#[test]
fn a_crash_outside_aterm_or_with_a_person_at_the_keys_is_not_relaunched() {
    let (rig, _) = rig_v1("crash-alone");
    let (mut h, _pty) = start_in_tab(&rig, None, &["first prompt"], &[]);
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert!(
        rig.journal_text().contains("no claim with relaunch scope"),
        "{}",
        rig.journal_text()
    );

    let (rig, _) = rig_v1("crash-person");
    let fake = aterm("crash-person");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    fake.set_status("human_ms", Some("5"));
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(
        rig.journal_text()
            .contains("not relaunched: aterm: person human_ms=5"),
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
    let bin = rig.path_dir.join("aterm");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit \"${{FAKE_ATERM_EXIT:-0}}\"\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let fake = aterm("crash-signin");
        let calls = fake_aterm_bin(&rig);
        std::fs::create_dir_all(login(&rig).parent().unwrap()).unwrap();
        std::fs::write(login(&rig), "{}").unwrap();
        let env = [("ATERM_CHILD", "1"), ("FAKE_ATERM_EXIT", exit)];
        let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &env);
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
    let fake = aterm("move-signin");
    // A screen only a measured Grok reader gives, so only the sign-in holds the move.
    reader_screen(&fake);
    let calls = fake_aterm_bin(&rig);
    std::fs::create_dir_all(login(&rig).parent().unwrap()).unwrap();
    std::fs::write(login(&rig), "{}").unwrap();
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
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
    journal_says(&rig, "would start signed out");
    // Six more ticks, each reading aterm again, are watched for a move.
    std::thread::sleep(ticks(6));
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

/// A CRASH IS RELAUNCHED ONLY ON A FRESH READ: a person who turns `[harness] relaunch` off
/// mid-session gets no relaunch, though the claim was made under `relaunch=true` and no event
/// said so (the read before the act sees it).
#[test]
fn a_crash_is_relaunched_only_when_a_fresh_read_still_allows_it() {
    let (rig, _) = rig_v1("crash-fresh");
    let fake = aterm("crash-fresh");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    fake.set_policy_quietly(&policy("relaunch=true", "relaunch=false"));
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(
        rig.journal_text()
            .contains("not relaunched: aterm: relaunch=false"),
        "{}",
        rig.journal_text()
    );
}

/// AUTOPILOT OFF GIVES THE CLAIM BACK: on aterm's `harness-policy` event with the policy off, the
/// harness unsets its claim and acts on nothing; back on, it claims again. A policy turned off
/// with no event is still caught by the fresh read before a crash relaunch.
#[test]
fn under_autopilot_off_the_claim_is_given_back_and_a_crash_is_not_relaunched() {
    let (rig, _) = rig_v1("autopilot-off");
    let fake = aterm("autopilot-off");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    assert!(
        wait_until(END, || fake.subscriptions() == 1),
        "the harness subscribes to the session's events"
    );
    // An event the harness reads past.
    fake.event("status program_state=-");
    let off = policy(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    );
    fake.set_policy(&off);
    assert!(
        wait_until(END, || fake.holder().is_none()),
        "the claim stands under Autopilot Off; journal:\n{}",
        rig.journal_text()
    );
    assert!(released(&fake, &holder), "{:?}", fake.verbs());
    journal_says(&rig, "RELEASED policy effective=off why=autopilot-off");
    // Back on: claimed again.
    fake.set_policy(POLICY_ON);
    assert!(
        wait_until(END, || fake.holder() == Some(holder.clone())),
        "not claimed again; journal:\n{}",
        rig.journal_text()
    );
    assert!(
        wait_until(END, || rig
            .journal_text()
            .matches(" CLAIMED holder=")
            .count()
            == 2),
        "{}",
        rig.journal_text()
    );
    // Off again with no event: the crash relaunch's fresh read sees it.
    fake.set_policy_quietly(&off);
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(
        rig.journal_text()
            .contains("not relaunched: aterm: policy effective=off why=autopilot-off"),
        "{}",
        rig.journal_text()
    );
}

/// A POLICY READ ATERM ANSWERS `ERR … retry` IS READ AGAIN UNTIL IT ANSWERS (Decision 41): the
/// read a `harness-policy` event asks for, answered with aterm's dialog `ERR`, is not thrown
/// away. The subscription stands and no other event comes, so the policy is read again on the
/// clock: a held claim is given back once the policy reads off, and a given-back claim made
/// again once it reads on.
#[test]
fn a_policy_read_aterm_answers_err_after_an_event_is_read_again_until_it_answers() {
    let (rig, _) = rig_v1("policy-err-event");
    let fake = aterm("policy-err-event");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    assert_eq!(fake.holder(), Some(holder.clone()));
    assert!(
        wait_until(END, || fake.subscriptions() == 1),
        "the harness subscribes to the session's events"
    );
    let reads = || {
        fake.verbs()
            .iter()
            .filter(|v| *v == "harness policy")
            .count()
    };
    for (body, event, back_on) in [
        (
            policy("effective=on why=enabled", "effective=off why=disabled"),
            "harness-policy gen=90",
            false,
        ),
        (POLICY_ON.to_string(), "harness-policy gen=91", true),
    ] {
        // The policy moves, but the read the event asks for meets the dialog.
        let before = reads();
        fake.set_policy_quietly(&body);
        fake.set_policy_reply(DIALOG);
        fake.event(event);
        assert!(
            wait_until(END, || reads() > before),
            "the event was not followed by a read"
        );
        fake.clear_policy_reply();
        let want = back_on.then(|| holder.clone());
        assert!(
            wait_until(END, || fake.holder() == want),
            "back_on={back_on}: the policy read was dropped; journal:\n{}",
            rig.journal_text()
        );
    }
    assert!(released(&fake, &holder), "{:?}", fake.verbs());
    assert_eq!(fake.subscriptions(), 1, "the subscription stood throughout");
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// THE SAFE-POINT READ FOLLOWS THE POLICY IT READS (Decision 42): while a newer build waits for
/// a safe point under a held claim, aterm is read on every tick, and a policy that went off with
/// no event (a session's own Autopilot word) is given back by that read itself; nothing moves.
#[test]
fn a_policy_going_off_while_a_newer_build_waits_gives_the_claim_back() {
    let (rig, _) = rig_v1("policy-off-waiting");
    let fake = aterm("policy-off-waiting");
    reader_screen(&fake);
    // A person typing: the newer build waits, and aterm is read on every tick.
    fake.set_status("human_ms", Some("5000"));
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["first prompt"],
        &[("GROK_ALAB_QUIET_MS", "0")],
    );
    let holder = format!("grok-alab@{}", h.pid);
    assert_eq!(fake.holder(), Some(holder.clone()));
    let id2 = rig.install(V2);
    rig.make_current(id2);
    journal_says(&rig, "wait aterm human_ms=5000");
    fake.set_policy_quietly(&policy(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    ));
    assert!(
        wait_until(END, || fake.holder().is_none()),
        "the claim lingers after the policy went off; journal:\n{}",
        rig.journal_text()
    );
    assert!(released(&fake, &holder), "{:?}", fake.verbs());
    journal_says(&rig, "RELEASED policy effective=off why=autopilot-off");
    std::thread::sleep(ticks(6));
    assert_eq!(
        rig.starts().len(),
        1,
        "nothing moved: {}",
        rig.journal_text()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// AN ATERM RESTART DROPS THE CLAIM, AND THE HARNESS MAKES IT AGAIN: the event stream's end
/// tells it at once, with no newer build waiting and no act to take, and the claim is made on a
/// fresh connection on the reclaim clock; grok is never touched.
#[test]
fn a_claim_aterm_dropped_is_made_again_without_waiting_for_an_act() {
    let (rig, _) = rig_v1("aterm-restart");
    let fake = aterm("aterm-restart");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    assert_eq!(fake.holder(), Some(holder.clone()));
    let connections = fake.connections();
    fake.restart();
    assert_eq!(fake.holder(), None);
    journal_says(&rig, " LOST ");
    assert!(
        wait_until(END, || fake.holder() == Some(holder.clone())),
        "the claim was not made again; journal:\n{}",
        rig.journal_text()
    );
    assert!(fake.connections() > connections, "a fresh connection");
    assert!(
        wait_until(END, || rig
            .journal_text()
            .matches(" CLAIMED holder=")
            .count()
            == 2),
        "{}",
        rig.journal_text()
    );
    assert_eq!(rig.starts().len(), 1, "grok was never touched");
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A SIGNED-OUT GROK IS NEVER RELAUNCHED: a relaunch is the harness's own act, and a signed-out
/// Grok Build opens xAI's sign-in page in the desk's browser. The crash ends the session, and
/// the person is told why and how to resume.
#[test]
fn a_crash_while_grok_is_signed_out_is_not_relaunched() {
    let (rig, _) = rig_v1("crash-signed-out");
    let fake = aterm("crash-signed-out");
    let (mut h, pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
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

/// A RELEASE ATERM DOES NOT TAKE NEVER LEAVES A CLAIM ON A KEPT CONNECTION (alab-wire Decision
/// 16, Decision 40 here): a claim made again on a standing connection that aterm does not prove
/// is given back, and when aterm refuses that release the claim may still stand on it. The
/// connection is dropped instead (ending the claim bound to it), and the claim made again on a
/// fresh one.
#[test]
fn a_claim_made_again_whose_release_aterm_refuses_drops_its_connection() {
    let (rig, _) = rig_v1("refused-release");
    let fake = aterm("refused-release");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    assert_eq!(fake.holder(), Some(holder.clone()));
    fake.set_policy(&policy(
        "effective=on why=enabled",
        "effective=off why=disabled",
    ));
    journal_says(&rig, "RELEASED");
    let connections = fake.connections();
    fake.set_proven(false);
    fake.set_release_reply(Some("ERR busy"));
    fake.set_policy(POLICY_ON);
    journal_says(&rig, "(release reply=ERR:busy)");
    assert!(
        rig.journal_text()
            .contains("LOST unproven supervisor=grok-alab@"),
        "{}",
        rig.journal_text()
    );
    fake.set_proven(true);
    fake.set_release_reply(None);
    assert!(
        wait_until(END, || fake.holder() == Some(holder.clone())
            && fake.connections() > connections),
        "never claimed again on a fresh connection; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// ATERM'S RESTART, ASKED: `EVENT restart cause=asked` ends grok with `SIGTERM` at once (no safe
/// point: the request is aterm's, or a person's through aterm) and resumes the same session on
/// the same build, naming the new child to aterm. It is no crash: the ladder is not touched.
#[test]
fn a_restart_aterm_asks_for_resumes_the_session_at_once() {
    let (rig, id) = rig_v1("restart-asked");
    let fake = aterm("restart-asked");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.send_restart("asked");
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no restart; journal:\n{}",
        rig.journal_text()
    );
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id);
    assert_eq!(second.argv, ["--resume", session.as_str()]);
    assert!(rig.log_text().contains(&format!("term build={id}")));
    let journal = rig.journal_text();
    assert!(
        journal.contains(&format!(
            "RESTART cause=asked: SIGTERM to grok 1.0.70; it resumes session {session} on it"
        )),
        "{journal}"
    );
    assert!(
        journal.contains("for aterm's restart cause=asked: --resume"),
        "{journal}"
    );
    assert!(!journal.contains("CRASH"), "{journal}");
    assert!(
        wait_until(END, || fake
            .child()
            .is_some_and(|c| c.starts_with(&format!("{}:", second.pid)))),
        "{:?}",
        fake.child()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A STALL'S RESTART IS MADE WHATEVER GROK DOES WITH ITS SIGTERM: a grok deaf to it gets
/// `SIGKILL` after the stall's grace (at most the exit grace), and the session resumes.
#[test]
fn a_stall_restart_kills_a_grok_deaf_to_its_sigterm() {
    let (rig, id) = rig_v1("restart-stall");
    let fake = aterm("restart-stall");
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["first prompt"],
        &[
            ("FAKE_GROK_MODE", "ignore-term"),
            ("GROK_ALAB_EXIT_GRACE_MS", "1000"),
        ],
    );
    let session = rig.starts()[0].argv[1].clone();
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.send_restart("stall");
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no restart; journal:\n{}",
        rig.journal_text()
    );
    assert!(rig.log_text().contains(&format!("term-ignored build={id}")));
    let journal = rig.journal_text();
    assert!(
        journal.contains(
            "RESTART cause=stall: grok did not end within 1000 ms of the SIGTERM; SIGKILL"
        ),
        "{journal}"
    );
    assert_eq!(rig.starts()[1].argv, ["--resume", session.as_str()]);
    // This grok ignores TERM too: the session ends with the harness's TERM passed on, and
    // then a KILL.
    let last = rig.starts()[1].pid;
    h.signal(libc::SIGTERM);
    journal_says(&rig, "TERM passed on to grok");
    signal_pid(last, libc::SIGKILL);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGKILL));
}

/// A RESTART ONTO ANOTHER MODEL: aterm names the model (`model=`), and Grok Build takes
/// `--model`, so the session resumes on it, every model option the launch kept replaced. An
/// ACCOUNT move is refused and grok runs on: grok-alab runs grok on the machine's one sign-in.
#[test]
fn a_model_restart_resumes_on_that_model_and_an_account_move_is_refused() {
    let (rig, _) = rig_v1("restart-model");
    let fake = aterm("restart-model");
    let (mut h, pty) = start_in_tab(&rig, Some(&fake), &["-m", "grok-3", "first prompt"], &[]);
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.send_account_restart("account", "work", None);
    journal_says(
        &rig,
        "RESTART-REFUSED cause=account to=work: grok-alab runs grok on the machine's one sign-in",
    );
    std::thread::sleep(ticks(6));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(exists(first.pid));
    fake.send_model_restart("model", "grok-4");
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no restart; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(
        rig.starts()[1].argv,
        ["--model=grok-4", "--resume", session.as_str()]
    );
    journal_says(&rig, "for aterm's restart cause=model model=grok-4");
    // The model stays for every later relaunch: a crash resumes on it too.
    wait_up(&pty, rig.starts()[1].pid);
    signal_pid(rig.starts()[1].pid, libc::SIGSEGV);
    assert!(
        wait_until(END, || rig.starts().len() == 3),
        "no crash relaunch; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(
        rig.starts()[2].argv,
        ["--model=grok-4", "--resume", session.as_str()]
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A RESTART WAITS FOR ATERM AT MOST ITS BOUND, AND NEVER OUTLIVES ITS CHILD: one aterm cannot
/// answer (a dialog's `ERR … retry`) waits and is refused once the bound has passed; and one
/// still waiting when its child is replaced (here by a crash relaunch) is passed over, never
/// applied to the new child.
#[test]
fn a_waiting_restart_is_bounded_and_passed_over_by_a_new_child() {
    let (rig, _) = rig_v1("restart-wait");
    let fake = aterm("restart-wait");
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["first prompt"],
        &[("GROK_ALAB_RESTART_WAIT_MS", "1500")],
    );
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.set_policy_reply(DIALOG);
    fake.send_restart("asked");
    journal_says(&rig, "RESTART-WAIT cause=asked aterm did not answer");
    journal_says(&rig, "RESTART-REFUSED cause=asked not made within 1500 ms");
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));

    let (rig, _) = rig_v1("restart-passed-over");
    let fake = aterm("restart-passed-over");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.set_policy_reply(DIALOG);
    fake.send_restart("asked");
    journal_says(&rig, "RESTART-WAIT cause=asked aterm did not answer");
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    journal_says(&rig, "CRASH-WAIT aterm did not answer");
    fake.clear_policy_reply();
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no crash relaunch; journal:\n{}",
        rig.journal_text()
    );
    journal_says(
        &rig,
        "RESTART-PASSED-OVER cause=asked it waited for the grok before this one",
    );
    std::thread::sleep(ticks(10));
    assert_eq!(rig.starts().len(), 2, "{}", rig.journal_text());
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A LATE EXIT FINISHES A MOVE ONLY WHEN NOTHING UNREAD COULD BE A PERSON'S (Decisions 22, 50):
/// under aterm the person's clock is aterm's `human_ms` as well as the terminal's, so after the
/// move's SIGTERM went unanswered, a claim lost (aterm restarted, and cannot take the claim yet)
/// or given back (Autopilot Off) leaves it unread, and grok's late exit 0 ends the session with
/// the resume line instead of moving it. CONTROL: with the claim standing and aterm reading
/// nobody at the keys, the same late exit finishes the move.
#[test]
fn a_late_exit_with_aterms_input_clock_unread_ends_the_session() {
    for case in ["standing", "lost", "given back"] {
        let (rig, id1) = rig_v1("late-unread");
        let fake = aterm("late-unread");
        let only_v1 = format!("FAKE_GROK_MODE_{id1}");
        let (mut h, pty) = start_in_tab(
            &rig,
            Some(&fake),
            &["first prompt"],
            &[
                ("GROK_ALAB_QUIET_MS", "0"),
                ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
                ("GROK_ALAB_EXIT_GRACE_MS", "300"),
                (only_v1.as_str(), "held-term"),
            ],
        );
        let session = rig.starts()[0].argv[1].clone();
        assert!(wait_until(END, || fake.subscriptions() == 1), "{case}");
        let id2 = rig.install(V2);
        rig.make_current(id2);
        journal_says(&rig, "SEEN grok 1.0.71");
        h.signal(libc::SIGUSR1);
        journal_says(&rig, "did not exit within 300 ms");
        match case {
            "lost" => {
                // aterm comes back busy, so the claim stays lost.
                fake.set_policy_reply(DIALOG);
                fake.restart();
                journal_says(&rig, " LOST ");
            }
            "given back" => {
                fake.set_policy(&policy(
                    "effective=on why=enabled",
                    "effective=off why=autopilot-off",
                ));
                journal_says(&rig, "RELEASED policy effective=off why=autopilot-off");
            }
            _ => {}
        }
        rig.release_term();
        if case == "standing" {
            assert!(
                wait_until(END, || rig.starts().len() == 2),
                "CONTROL: the late exit did not finish the move; journal:\n{}",
                rig.journal_text()
            );
            assert_eq!(rig.starts()[1].build, id2);
            journal_says(&rig, "after a late exit");
            assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
            continue;
        }
        assert_eq!(
            exit_code(h.wait_end(END)),
            Some(0),
            "{case}: journal:\n{}",
            rig.journal_text()
        );
        assert_eq!(rig.starts().len(), 1, "{case}: {}", rig.journal_text());
        assert!(
            rig.journal_text().contains(
                "not relaunched: a person may have typed since: aterm's input clock was not read"
            ),
            "{case}: {}",
            rig.journal_text()
        );
        let hint = format!("grok --resume {session}");
        assert!(
            wait_until(END, || String::from_utf8_lossy(&pty.output.lock().unwrap())
                .contains(&hint)),
            "{case}: {:?}",
            String::from_utf8_lossy(&pty.output.lock().unwrap())
        );
    }
}

/// A RESTART ATERM ASKS FOR NEVER ENDS A GROK IT CANNOT START AGAIN (Decision 51): with the
/// running build gone from the store (atpkg removed it after a newer install), the restart is
/// refused before any signal and grok runs on, as for a machine signed out; a session aterm
/// asked to restart is never ended for want of the build it ran.
#[test]
fn a_restart_is_refused_when_the_running_build_is_gone_from_the_store() {
    let (rig, id) = rig_v1("restart-gone");
    let fake = aterm("restart-gone");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let first = rig.starts()[0].clone();
    assert!(wait_until(END, || fake.subscriptions() == 1));
    let pd = rig.program_dir();
    std::fs::remove_dir_all(pd.join(id.to_string())).unwrap();
    std::fs::remove_file(pd.join(format!("{id}.ready"))).unwrap();
    fake.send_restart("asked");
    journal_says(
        &rig,
        &format!(
            "RESTART-REFUSED cause=asked: grok 1.0.70 (build {id}) could not be started again: \
             it is no longer ready in the store"
        ),
    );
    std::thread::sleep(ticks(6));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    assert!(h.alive() && exists(first.pid));
    assert!(
        !rig.log_text().contains(&format!("term build={id}")),
        "grok was sent its SIGTERM: {}",
        rig.log_text()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A MOVE FINISHES ONLY ONTO A BUILD THE GRANT STILL ALLOWS (Decision 52): a person's cap that
/// withdraws `upgrade` while grok is still answering the move's SIGTERM hands moves back to
/// aterm's engine, so grok's exit resumes the session on the build it ran, not the newer one,
/// as a crash relaunch without `upgrade` does. (Every other move test is the control: with the
/// grant intact, the exit relaunches onto the newer build.)
#[test]
fn a_cap_that_withdraws_upgrade_mid_move_resumes_on_the_running_build() {
    let (rig, id1) = rig_v1("move-capped");
    let fake = aterm("move-capped");
    let only_v1 = format!("FAKE_GROK_MODE_{id1}");
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["first prompt"],
        &[
            ("GROK_ALAB_QUIET_MS", "0"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "0"),
            ("GROK_ALAB_EXIT_GRACE_MS", "20000"),
            (only_v1.as_str(), "held-term"),
        ],
    );
    let session = rig.starts()[0].argv[1].clone();
    assert!(wait_until(END, || fake.subscriptions() == 1));
    let id2 = rig.install(V2);
    rig.make_current(id2);
    journal_says(&rig, "SEEN grok 1.0.71");
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig
            .log_text()
            .contains(&format!("term-held build={id1}"))),
        "no move's SIGTERM; journal:\n{}",
        rig.journal_text()
    );
    fake.set_cap(Some(&[Scope::Relaunch]));
    journal_says(&rig, "NARROWED scope=relaunch");
    rig.release_term();
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch; journal:\n{}",
        rig.journal_text()
    );
    let second = rig.starts()[1].clone();
    assert_eq!(
        second.build,
        id1,
        "moved onto the newer build without upgrade: {}",
        rig.journal_text()
    );
    assert_eq!(second.argv, ["--resume", session.as_str()]);
    journal_says(
        &rig,
        "HOLD aterm no longer grants upgrade (scope=relaunch); grok resumes on 1.0.70, not 1.0.71",
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// ATERM'S OWN MARKS NAME AN ATERM TAB (Decision 53): `ATERM_SESSION_ID` alone counts, as
/// `TERM_PROGRAM=aterm` and `ATERM_CHILD=1` do (aterm's agent brief, `alab-wire`'s
/// `EnvNames`), so a launch that names no session aterm can be found by passes through, as any
/// tab without a socket does. CONTROL: with no mark at all the harness supervises grok
/// outside aterm (Decision 13).
#[test]
fn aterm_session_id_alone_marks_an_aterm_tab_and_a_tab_with_no_session_passes_through() {
    let (rig, _) = rig_v1("session-id-only");
    let (mut h, _pty) = start_in_tab(&rig, None, &["hi"], &[("ATERM_SESSION_ID", "s-1a")]);
    assert_eq!(
        rig.starts()[0].pid,
        h.pid,
        "grok runs in the harness's place"
    );
    assert!(
        rig.journal_text()
            .contains("(exec): aterm did not take the claim (locator"),
        "{}",
        rig.journal_text()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));

    let (rig, _) = rig_v1("no-marks");
    let (mut h, _pty) = start_in_tab(&rig, None, &["hi"], &[]);
    assert_ne!(
        rig.starts()[0].pid,
        h.pid,
        "CONTROL: supervised outside aterm"
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// A CRASH WHILE THE CLAIM IS LOST WAITS FOR IT, WITHIN THE AWAY BUDGET: a grok that crashes
/// while an aterm restart has dropped the claim waits (`CRASH-WAIT … link-lost`) and is
/// relaunched once the claim is made again on the reclaim clock, on aterm's fresh word. One
/// whose claim is not proven again within the budget (aterm's 45 s, shortened to 1.5 s by this
/// test build's `GROK_ALAB_TEST_AWAY_BUDGET_MS`) is passed on as the session's end, with the
/// crash's own status.
#[test]
fn a_crash_while_the_claim_is_lost_waits_for_it_within_the_away_budget() {
    let (rig, id) = rig_v1("crash-lost");
    let fake = aterm("crash-lost");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    let session = rig.starts()[0].argv[1].clone();
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.restart();
    journal_says(&rig, " LOST ");
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    journal_says(&rig, "CRASH-WAIT aterm did not answer (link-lost)");
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch once the claim was made again; journal:\n{}",
        rig.journal_text()
    );
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id);
    assert_eq!(second.argv, ["--resume", session.as_str()]);
    assert_eq!(fake.holder(), Some(holder));
    let journal = rig.journal_text();
    let claimed = journal.find(" CLAIMED holder=grok-alab@").unwrap();
    let again = journal.rfind(" CLAIMED holder=grok-alab@").unwrap();
    let relaunched = journal.find("RELAUNCHED after a crash").unwrap();
    assert!(claimed < again && again < relaunched, "{journal}");
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));

    let (rig, _) = rig_v1("crash-lost-spent");
    let fake = aterm("crash-lost-spent");
    let (mut h, _pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["first prompt"],
        &[(grok_alab::supervise::TEST_AWAY_BUDGET_ENV, "1500")],
    );
    assert!(wait_until(END, || fake.subscriptions() == 1));
    // aterm comes back busy, so the claim is not made again.
    fake.set_policy_reply(DIALOG);
    fake.restart();
    journal_says(&rig, " LOST ");
    signal_pid(rig.starts()[0].pid, libc::SIGSEGV);
    assert_eq!(
        term_signal(h.wait_end(END)),
        Some(libc::SIGSEGV),
        "journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    let journal = rig.journal_text();
    assert!(
        journal.contains(
            "CRASH-WAIT aterm did not answer (link-lost); asked again within the 1500 ms budget"
        ),
        "{journal}"
    );
    assert!(
        journal.contains("not relaunched: claim-not-proven-again link-lost"),
        "{journal}"
    );
}

/// THE EVENT STREAM LOST ALONE IS REOPENED, AND THE POLICY READ WHILE IT IS DOWN (alab-wire
/// Decision 18): aterm drops the harness's subscription while the claim's own connection stays,
/// so the claim stands (`EVENTS-LOST`, never `LOST`). While aterm refuses to subscribe it again,
/// a person's Autopilot Off, which no event would announce, still gives the claim back within
/// one 5 s reopen attempt, and back on the claim is made again; once aterm serves the
/// subscription the stream is reopened (`EVENTS-REOPENED`), and a restart aterm asks for on it
/// is made.
#[test]
fn an_event_stream_lost_alone_is_reopened_and_the_policy_read_while_it_is_down() {
    let (rig, _) = rig_v1("events-lost");
    let fake = aterm("events-lost");
    let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[]);
    let holder = format!("grok-alab@{}", h.pid);
    assert!(wait_until(END, || fake.subscriptions() == 1));
    fake.set_subscribe_reply(Some("ERR busy"));
    fake.end_events();
    journal_says(&rig, " EVENTS-LOST closed");
    assert_eq!(fake.holder(), Some(holder.clone()), "the claim stands");
    fake.set_policy_quietly(&policy(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    ));
    assert!(
        wait_until(END, || fake.holder().is_none()),
        "the claim lingers while the stream is down; journal:\n{}",
        rig.journal_text()
    );
    journal_says(&rig, "RELEASED policy effective=off why=autopilot-off");
    fake.set_policy_quietly(POLICY_ON);
    assert!(
        wait_until(END, || fake.holder() == Some(holder.clone())),
        "not claimed again; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(fake.subscriptions(), 1, "every reopen was refused");
    fake.set_subscribe_reply(None);
    journal_says(&rig, " EVENTS-REOPENED");
    assert_eq!(fake.subscriptions(), 2);
    assert!(
        !rig.journal_text().contains(" LOST "),
        "the claim's connection was never lost: {}",
        rig.journal_text()
    );
    fake.send_restart("asked");
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "the reopened stream's restart was not made; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(exit_code(end(&mut h, &rig)), Some(0));
}

/// THE LADDER'S LATER STEPS AND ITS FORGIVENESS, END TO END (this test build's
/// `GROK_ALAB_TEST_LADDER` seeds the streak): with two relaunches standing a crash waits 60 s,
/// with three or more 10 minutes, and a `TERM` in the pause ends the session with the crash's own
/// status. A streak whose last relaunch has run healthy for 10 minutes is forgiven: the crash is
/// relaunched after 1 s, and a crash at once after that relaunch waits the second step, 10 s,
/// since the relaunch began a new healthy run.
#[test]
fn the_ladders_later_steps_wait_60_s_then_10_min_and_a_healthy_run_forgives_it() {
    let ladder = grok_alab::supervise::TEST_LADDER_ENV;
    for (seed, relaunch, pause) in [("2,0", 3, 60_000), ("3,0", 4, 600_000), ("7,0", 8, 600_000)] {
        let (rig, _) = rig_v1("ladder-step");
        let fake = aterm("ladder-step");
        let (mut h, _pty) = start_in_tab(&rig, Some(&fake), &["first prompt"], &[(ladder, seed)]);
        let first = rig.starts()[0].clone();
        let session = first.argv[1].clone();
        signal_pid(first.pid, libc::SIGSEGV);
        journal_says(
            &rig,
            &format!("relaunch {relaunch} on session {session} in {pause} ms"),
        );
        std::thread::sleep(ticks(6));
        assert_eq!(rig.starts().len(), 1, "{seed}: {}", rig.journal_text());
        h.signal(libc::SIGTERM);
        assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV), "{seed}");
    }

    // 1.5 s short of a healthy run at the start; the crash comes 3 s later.
    let (rig, _) = rig_v1("ladder-forgiven");
    let fake = aterm("ladder-forgiven");
    let (mut h, pty) = start_in_tab(
        &rig,
        Some(&fake),
        &["first prompt"],
        &[(ladder, "3,598500")],
    );
    journal_says(&rig, "crash ladder starts at streak 3");
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    std::thread::sleep(Duration::from_secs(3));
    signal_pid(first.pid, libc::SIGSEGV);
    journal_says(&rig, &format!("relaunch 1 on session {session} in 1000 ms"));
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch; journal:\n{}",
        rig.journal_text()
    );
    let second = rig.starts()[1].clone();
    wait_up(&pty, second.pid);
    signal_pid(second.pid, libc::SIGSEGV);
    journal_says(
        &rig,
        &format!("relaunch 2 on session {session} in 10000 ms"),
    );
    h.signal(libc::SIGTERM);
    assert_eq!(term_signal(h.wait_end(END)), Some(libc::SIGSEGV));
    assert_eq!(rig.starts().len(), 2, "{}", rig.journal_text());
}
