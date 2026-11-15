// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SUPERVISING LOOP: run the vendor build as a child on the inherited terminal, pass its
//! signals on, follow its stops, move it onto a newer build at a safe point, and end exactly
//! as it ended.
//!
//! The child is spawned in the harness's own process group (no `setpgid`), with the
//! terminal inherited and nothing proxied: the harness leads the foreground group, and the
//! child reads and draws the terminal itself. The harness writes nothing to the terminal
//! while a child runs; what it does goes to the journal ([`crate::journal`]).
//!
//! Under aterm the loop runs the session's [`Link`] (`alab-wire`, [`crate::aterm`]) on every
//! tick and wakes for its event stream: the link claims again when it was lost, follows the
//! policy on its `harness-policy` event (the claim given back while it is off) and hands over
//! the restarts aterm asks for. With a standing claim that takes `upgrade`, aterm's safe-point
//! read is the witness on every tick a newer build waits ([`safepoint::Witness`]). Every child
//! start is announced to aterm (`child=<pid>:<start>` under contract 4), and a restart aterm
//! asked for before it is passed over, never applied to the new child. A child that ends
//! abnormally (`alab_wire::relaunch::abnormal_exit`) under a claim is relaunched on aterm's
//! word and back-off ([`Ladder`]); a restart aterm asks for (`restart cause=asked|stall`, with
//! `model=` when it names one) ends grok and resumes the same session at once
//! ([`restart_now`]).
//!
//! Every relaunch is the harness's own act, so none starts a grok that would start signed out
//! ([`crate::credentials`]): under aterm a signed-out relaunch is signed in first through aterm's
//! one sign-in for the machine ([`crate::signin`], Decision 26), and ends the session only when
//! that fails. A late answer to the harness's own SIGTERM finishes the move when
//! nobody typed since ([`ended_by_term`], [`LATE_EXIT_WINDOW`]), and a new build that refuses to
//! start sends the session back to the build it moved from ([`refused_start`]).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd as _;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use alab_wire::claim::Human;
use alab_wire::link::{LATER_RETRY_MS, RECLAIM_EVERY_MS};
use alab_wire::relaunch::{AWAY_BUDGET, AwayBudget, ChildEnd, Ladder};
use alab_wire::{Child, ChildProcess, Link, Reading, Relaunch, Restart, Scope, Verdict};
use rustix::process::getpgrp;
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetpgrp, tcsetattr};

use crate::aterm;
use crate::env::Env;
use crate::journal::Journal;
use crate::lanes::{Carried, Plan};
use crate::procs;
use crate::safepoint::{self, Decision, Facts, Witness};
use crate::signals::{self, Signals};
use crate::store::{self, Build, Layout};

/// The variable the child finds the harness's pid in (for `kill -USR1 "$GROK_ALAB_PID"`).
pub const PID_ENV: &str = "GROK_ALAB_PID";

/// The variable the child finds how many harnesses run above it in (this one's depth plus
/// one; [`crate::nesting`]).
pub const DEPTH_ENV: &str = "GROK_ALAB_DEPTH";

/// Overrides, in milliseconds, of the loop's timings.
pub const POLL_ENV: &str = "GROK_ALAB_POLL_MS";
/// See [`POLL_ENV`].
pub const QUIET_ENV: &str = "GROK_ALAB_QUIET_MS";
/// See [`POLL_ENV`].
pub const HUMAN_GRACE_ENV: &str = "GROK_ALAB_HUMAN_GRACE_MS";
/// See [`POLL_ENV`].
pub const EXIT_GRACE_ENV: &str = "GROK_ALAB_EXIT_GRACE_MS";
/// See [`POLL_ENV`].
pub const MIN_RUN_ENV: &str = "GROK_ALAB_MIN_RUN_MS";
/// See [`POLL_ENV`].
pub const RESTART_WAIT_ENV: &str = "GROK_ALAB_RESTART_WAIT_MS";

/// A debug build's shorter away budget for a crash relaunch, in milliseconds (100 up to
/// aterm's 45000): its tests see a crash passed on once aterm stays away past it without
/// waiting 45 s. A release build never reads it (alab-harness Decision 17).
pub const TEST_AWAY_BUDGET_ENV: &str = "GROK_ALAB_TEST_AWAY_BUDGET_MS";

/// A debug build's crash ladder at the session's start, `<streak>,<since ms>`: `streak` crash
/// relaunches stand, the last made `since` ago (`Ladder::carried`). Its tests reach the
/// ladder's 60 s and 10 min steps and its healthy-run forgiveness without crashing that many
/// times or waiting 10 minutes. A release build never reads it.
pub const TEST_LADDER_ENV: &str = "GROK_ALAB_TEST_LADDER";

/// How soon after its `SIGTERM` a grok aterm asked to restart for a stall gets `SIGKILL` (at
/// most the exit grace): aterm's own stall remedy kills a stalled program, and a stalled grok
/// will not answer a `SIGTERM` in good time.
pub const STALL_KILL_AFTER: Duration = Duration::from_secs(5);

/// How soon aterm is asked again about a crash or an asked restart while it could not answer.
const ASK_AGAIN: Duration = Duration::from_millis(LATER_RETRY_MS);

/// The loop's timings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How often the store's `current` link is read, and aterm with it.
    pub poll: Duration,
    /// The output quiet a move needs ([`safepoint::DEFAULT_QUIET`]).
    pub quiet: Duration,
    /// The input quiet a move needs ([`safepoint::DEFAULT_HUMAN_GRACE`]).
    pub human_grace: Duration,
    /// How long the child has to exit after the harness's own SIGTERM: for a move, before the
    /// move is abandoned (a move never kills harder); for a restart aterm asked for, before the
    /// `SIGKILL` (sooner for a stall, [`STALL_KILL_AFTER`]).
    pub exit_grace: Duration,
    /// How long after a move's relaunch an abnormal end reads as the new build refusing to
    /// start, which sends the session back to the build it moved from ([`refused_start`]).
    pub min_run: Duration,
    /// How long a restart aterm asked for may wait, from its arrival, for aterm to answer
    /// before it is refused ([`AWAY_BUDGET`]).
    pub restart_wait: Duration,
    /// How long a crash relaunch waits in all for aterm to answer again, or for a lost claim
    /// to be made again ([`AWAY_BUDGET`]; shorter only in a debug build,
    /// [`TEST_AWAY_BUDGET_ENV`]).
    pub away_budget: Duration,
    /// The crash ladder the session starts with: `None` for an empty one, and only a debug
    /// build sets it ([`TEST_LADDER_ENV`]).
    pub ladder: Option<(u32, Duration)>,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            poll: Duration::from_secs(1),
            quiet: safepoint::DEFAULT_QUIET,
            human_grace: safepoint::DEFAULT_HUMAN_GRACE,
            exit_grace: Duration::from_secs(10),
            min_run: Duration::from_secs(10),
            restart_wait: AWAY_BUDGET,
            away_budget: AWAY_BUDGET,
            ladder: None,
        }
    }
}

impl Timing {
    /// The defaults with the overrides `env` sets; a value that is not a number in range is
    /// ignored (and journaled). A debug build also reads [`TEST_AWAY_BUDGET_ENV`] and
    /// [`TEST_LADDER_ENV`]; a release build answers aterm with aterm's own budget and ladder.
    #[must_use]
    pub fn from_env(env: &Env, journal: &Journal) -> Timing {
        let d = Timing::default();
        let ms = |name: &str, min: u64, max: u64, default: Duration| -> Duration {
            let Some(raw) = env.get(name) else {
                return default;
            };
            match raw.to_str().and_then(|s| s.trim().parse::<u64>().ok()) {
                Some(v) if (min..=max).contains(&v) => Duration::from_millis(v),
                _ => {
                    journal.line(
                        "NOTE",
                        &format!(
                            "{name} is not a number of ms in {min}..={max}; the default stands"
                        ),
                    );
                    default
                }
            }
        };
        let test_build = cfg!(debug_assertions);
        let away_budget = if test_build {
            let most = u64::try_from(AWAY_BUDGET.as_millis()).unwrap_or(u64::MAX);
            ms(TEST_AWAY_BUDGET_ENV, 100, most, d.away_budget)
        } else {
            d.away_budget
        };
        let ladder = if test_build {
            env.get(TEST_LADDER_ENV)
                .and_then(|raw| match raw.to_str().and_then(test_ladder) {
                    Some(ladder) => Some(ladder),
                    None => {
                        journal.line(
                            "NOTE",
                            &format!(
                                "{TEST_LADDER_ENV} is not <streak>,<since ms>; the ladder starts empty"
                            ),
                        );
                        None
                    }
                })
        } else {
            d.ladder
        };
        Timing {
            poll: ms(POLL_ENV, 20, 60_000, d.poll),
            quiet: ms(QUIET_ENV, 0, 86_400_000, d.quiet),
            human_grace: ms(HUMAN_GRACE_ENV, 0, 86_400_000, d.human_grace),
            exit_grace: ms(EXIT_GRACE_ENV, 100, 600_000, d.exit_grace),
            min_run: ms(MIN_RUN_ENV, 0, 600_000, d.min_run),
            restart_wait: ms(RESTART_WAIT_ENV, 100, 3_600_000, d.restart_wait),
            away_budget,
            ladder,
        }
    }
}

/// [`TEST_LADDER_ENV`]'s value: `<streak>,<since ms>`, each a decimal.
fn test_ladder(raw: &str) -> Option<(u32, Duration)> {
    let (streak, since) = raw.trim().split_once(',')?;
    Some((
        streak.trim().parse().ok()?,
        Duration::from_millis(since.trim().parse().ok()?),
    ))
}

/// Everything one supervised session needs.
#[derive(Debug)]
pub struct Session<'a> {
    /// The package store.
    pub layout: &'a Layout,
    /// The build the first child runs.
    pub build: Build,
    /// The launch's own arguments (after `argv[0]`).
    pub args: Vec<OsString>,
    /// What the harness makes of them.
    pub plan: Plan,
    /// Where the harness says what it did.
    pub journal: &'a Journal,
    /// The loop's timings.
    pub timing: Timing,
    /// The installed signal handlers.
    pub signals: &'a Signals,
    /// How many harnesses run above this one ([`crate::nesting`]).
    pub depth: u32,
    /// The session's link to aterm, as the launch left it: absent outside aterm, held, or lost
    /// and claimed again on the clock ([`crate::under_aterm`]).
    pub link: Link,
}

/// A move onto a newer build, or a restart aterm asked for, under way: the `SIGTERM` is sent,
/// and the relaunch waits for the child's end.
#[derive(Debug)]
struct Moving {
    /// When the last signal was sent. A continue starts it again (Decision 39).
    sent: Instant,
    /// The build to relaunch on: the newer one for a move, the running one for a restart.
    target: Build,
    /// For a restart aterm asked for, how it ends and what it resumes; `None` for a move, which
    /// never kills harder (Decision 8).
    asked: Option<Asked>,
}

/// A restart aterm asked for, under way.
#[derive(Debug)]
struct Asked {
    /// Its journal words: `cause=<cause>[ model=<model>]`.
    words: String,
    /// The session it resumes, on the model aterm named when it named one.
    carried: Carried,
    /// How long after the `SIGTERM` the `SIGKILL` follows ([`Timing::exit_grace`], or
    /// [`STALL_KILL_AFTER`] for a stall).
    kill_after: Duration,
    /// Whether the `SIGKILL` was sent.
    killed: bool,
}

/// A restart aterm asked for that waits for aterm to answer. It waits at most
/// [`Timing::restart_wait`] from its arrival: a restart is made for the moment aterm asked in,
/// and aterm asks again while its cause stands (a stall's remedy fires again).
#[derive(Debug)]
struct Deferred {
    event: Restart,
    /// When it is refused if it still waits.
    expires: Instant,
    /// When aterm is asked again.
    next_ask: Instant,
    /// Why it waits, journaled when it changes.
    why: String,
}

/// What a restart aterm asked for came to.
#[derive(Debug)]
enum Answer {
    /// grok was sent `SIGTERM`; the restart is under way.
    Moving(Moving),
    /// It was refused, and journaled.
    Refused,
    /// aterm did not answer; why.
    Waits(String),
}

/// `restart`'s journal words: `cause=<cause>`, and the model and account it names.
fn restart_words(event: &Restart) -> String {
    let mut words = format!("cause={}", aterm_word(&event.cause));
    if let Some(model) = &event.model {
        words.push_str(" model=");
        words.push_str(&aterm_word(model));
    }
    if let Some(account) = &event.account {
        words.push_str(" to=");
        words.push_str(&aterm_word(&account.label));
    }
    words
}

/// A word aterm sent, made safe for one journal line.
fn aterm_word(word: &str) -> String {
    alab_wire::claim::journal_word(word)
}

/// What the claim names for the child now: the running build and, while a child runs, its
/// kernel identity.
fn claim_child(running: &Build, process: Option<ChildProcess>) -> Child<'_> {
    aterm::child(running.version.as_deref(), process)
}

/// The link's own clock, run on every pass of the loop: claim a lost link again, follow the
/// policy, and drain the event stream. The restart aterm asked for, if one came.
fn link_tick(link: &mut Link, child: Child<'_>, journal: &Journal) -> Option<Restart> {
    let now = aterm::now_ms();
    link.tick(child, now);
    let drained = link.events(child, now);
    aterm::journal(link, journal);
    drained.restart
}

/// A child was just started: a restart aterm asked for until now was meant for the child
/// before it, so it is passed over and never applied to this one (`Link::events`), the one
/// still waiting and one aterm sent as that child ended. Then announce the new child to aterm
/// (`child=<pid>:<start>` under contract 4; the reply is the grant from then on).
fn announce(link: &mut Link, child: Child<'_>, deferred: &mut Option<Deferred>, journal: &Journal) {
    if let Some(waiting) = deferred.take() {
        journal.line(
            "RESTART-PASSED-OVER",
            &format!(
                "{} it waited for the grok before this one",
                restart_words(&waiting.event)
            ),
        );
    }
    if let Some(event) = link_tick(link, child, journal) {
        journal.line(
            "RESTART-PASSED-OVER",
            &format!(
                "{} it came before this grok was announced",
                restart_words(&event)
            ),
        );
    }
    link.reannounce(child, aterm::now_ms());
    aterm::journal(link, journal);
}

/// The start line's word on aterm.
fn supervision(link: &Link) -> String {
    if let Some(grant) = link.grant() {
        format!(
            "{} holds the session under aterm contract {}, scope={} revision={} (policy followed {})",
            link.config().holder(std::process::id()),
            grant.contract.number(),
            grant.words(),
            grant.revision_word(),
            if link.event_fd().is_some() {
                "on its harness-policy event"
            } else {
                "on a clock: aterm gave no event stream"
            }
        )
    } else if link.is_lost() {
        format!(
            "no claim yet: aterm could not take it at the launch; it is claimed again every {} s",
            RECLAIM_EVERY_MS / 1000
        )
    } else {
        "not under aterm (SIGUSR1 asks for a move)".to_string()
    }
}

/// aterm's word on a move a person asked for by hand (`SIGUSR1`): under a standing claim, one
/// fresh read, judged by [`aterm::unheld`] (the claim still this harness's with `upgrade`, no
/// halt, no other driver's hand); a link that fails on it holds the move. On a link lost
/// before, the hold aterm last stated stands (`Link::lost_hold`). With no claim standing and
/// no such hold, the person's word stands on the harness's own readings.
fn person_word(link: &mut Link, journal: &Journal) -> Result<(), String> {
    let word = if link.judges() {
        let holder = link.config().holder(std::process::id());
        match link.read(aterm::now_ms()) {
            Reading::Fresh(facts) => aterm::unheld(&facts, &holder),
            Reading::Hold(why) => Err(why),
            Reading::Local if link.is_lost() => Err(link
                .lost_hold()
                .unwrap_or_else(|| "aterm's connection failed".to_string())),
            Reading::Local => Ok(()),
        }
    } else {
        link.lost_hold().map_or(Ok(()), Err)
    };
    aterm::journal(link, journal);
    word
}

/// aterm's safe-point read, the witness under a standing claim: one fresh policy and status
/// pair judged by `alab-wire` (`Link::safe_point`, aterm's own grace).
fn aterm_witness(link: &mut Link, journal: &Journal) -> Witness {
    let verdict = link.safe_point(Duration::ZERO, aterm::now_ms());
    aterm::journal(link, journal);
    Witness::Aterm(match verdict {
        Verdict::Safe => Ok(()),
        Verdict::Hold(why) => Err(why),
        Verdict::Local => Err("no read of aterm: its claim was lost or given back".to_string()),
    })
}

/// Why a person may have typed in the last `since` (input after the harness's own SIGTERM),
/// or `None` when nothing says so: the terminal's input clock always, and under aterm aterm's
/// `human_ms` on one fresh read of the standing claim. Anything unread counts as typed
/// (Decision 22): a claim lost, given back or not yet made leaves aterm's clock unread, as does
/// a read aterm answers with a hold, so only a session with no link to aterm (outside aterm)
/// is judged on the terminal's clock alone (Decision 50).
fn typed_since(link: &mut Link, since: Duration, journal: &Journal) -> Option<String> {
    let tty_says =
        procs::tty_quiet(io::stdin().as_fd(), SystemTime::now()).is_none_or(|q| q.input < since);
    if tty_says {
        return Some("a person may have typed since (the terminal's input clock)".to_string());
    }
    if link.is_absent() {
        return None;
    }
    let reading = link.read(aterm::now_ms());
    aterm::journal(link, journal);
    let unread = |why: &str| {
        Some(format!(
            "a person may have typed since: aterm's input clock was not read ({why})"
        ))
    };
    match reading {
        Reading::Fresh(facts) => match facts.status.human {
            Human::Never => None,
            Human::Ago(ago) if ago >= since => None,
            Human::Ago(ago) => Some(format!(
                "a person may have typed since (aterm human_ms={})",
                ago.as_millis()
            )),
            Human::Unknown => unread("human_ms unknown"),
        },
        Reading::Hold(why) => unread(&why),
        Reading::Local => unread("no standing claim: it is lost, given back or not yet made"),
    }
}

/// aterm asked the claim holder to restart its child (`restart cause=<cause>`): end grok with
/// `SIGTERM` now (and `SIGKILL` if it does not end, [`Asked::kill_after`]), and resume the same
/// session on the running build once it has ended. The request is aterm's, or a person's
/// through aterm, so no safe point is waited for, and it is no crash: the ladder is not
/// touched.
///
/// It is made only when the launch is carried, the restart names no account (grok-alab runs
/// grok on the machine's one sign-in: aterm moves accounts by `to=` only for a vendor whose
/// account lives in a home of its own, and Grok's pool seam is the login file
/// `GROK_AUTH_PATH` it reads at start), the running build is still ready in the store (atpkg
/// may have removed it since it started, and a grok ended for a restart that cannot start
/// again would end the session, Decision 51), it would start signed in, and one fresh policy
/// and status read permits it (`Link::relaunch_word` with `asked`). A restart naming a
/// model resumes on it: Grok Build takes `--model` ([`Carried::with_model`]). One aterm could
/// not answer waits ([`Deferred`]).
#[allow(clippy::too_many_arguments)]
fn restart_now(
    link: &mut Link,
    event: &Restart,
    layout: &Layout,
    running: &Build,
    carried: Option<&Carried>,
    child: libc::pid_t,
    timing: &Timing,
    journal: &Journal,
) -> Answer {
    let words = restart_words(event);
    let refuse = |why: &str| {
        journal.line("RESTART-REFUSED", &format!("{words}: {why}"));
        Answer::Refused
    };
    let Some(carried) = carried else {
        return refuse("this launch cannot be carried, so grok could not be resumed");
    };
    if event.account.is_some() {
        return refuse(
            "grok-alab runs grok on the machine's one sign-in and cannot move it to another \
             account; grok runs on",
        );
    }
    if let Err(why) = startable(layout, running.id) {
        return refuse(&format!(
            "grok {} (build {}) could not be started again: {why}; grok runs on",
            running.version_label(),
            running.id
        ));
    }
    if !signed_in_for(layout, running) {
        return refuse(
            "grok would start signed out (no GROK_AUTH, XAI_API_KEY or login file it accepts); \
             grok runs on",
        );
    }
    let carried = match &event.model {
        Some(model) => carried.with_model(model),
        None => carried.clone(),
    };
    let word = link.relaunch_word(true, aterm::now_ms());
    aterm::journal(link, journal);
    match word {
        Relaunch::Permitted => {}
        Relaunch::Denied(why) => return refuse(&format!("aterm: {why}")),
        Relaunch::Away(why) | Relaunch::Later(why) => {
            return Answer::Waits(format!("aterm did not answer: {why}"));
        }
    }
    if !signals::send(child, libc::SIGTERM) {
        return refuse("the kernel refused the SIGTERM; grok runs on");
    }
    journal.line(
        "RESTART",
        &format!(
            "{words}: SIGTERM to grok {}; it resumes session {} on it",
            running.version_label(),
            carried.session
        ),
    );
    let kill_after = if event.cause == "stall" {
        STALL_KILL_AFTER.min(timing.exit_grace)
    } else {
        timing.exit_grace
    };
    Answer::Moving(Moving {
        sent: Instant::now(),
        target: running.clone(),
        asked: Some(Asked {
            words,
            carried,
            kill_after,
            killed: false,
        }),
    })
}

/// Whether build `id` can be started now: still ready in the store with its executable
/// (`store::build`), and not this harness itself ([`crate::is_self`]).
///
/// # Errors
/// Why it cannot, in a few words.
fn startable(layout: &Layout, id: u64) -> Result<Build, String> {
    match store::build(layout, id) {
        Ok(b) if crate::is_self(&b.bin) => Err(format!("{} is this harness", b.bin.display())),
        Ok(b) => Ok(b),
        Err(store::Missing::NotReady(marker)) => Err(format!(
            "it is no longer ready in the store ({} does not say ok)",
            marker.display()
        )),
        Err(store::Missing::NoBinary(bin)) => {
            Err(format!("it has no executable {} now", bin.display()))
        }
        Err(other) => Err(other.to_string()),
    }
}

/// A wait status as `alab-wire` judges a child's end.
fn child_end(status: libc::c_int) -> ChildEnd {
    if libc::WIFEXITED(status) {
        ChildEnd::Exited(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        ChildEnd::Signaled(libc::WTERMSIG(status))
    } else if libc::WIFSTOPPED(status) {
        ChildEnd::Stopped(libc::WSTOPSIG(status))
    } else {
        ChildEnd::Continued
    }
}

/// Whether `sig`, caught while no grok runs, ends a crash's wait: whoever sends it wants the
/// session over, and a relaunch would fight them.
const fn ends_a_crash(sig: libc::c_int) -> bool {
    matches!(
        sig,
        libc::SIGINT | libc::SIGQUIT | libc::SIGHUP | libc::SIGTERM
    )
}

/// The session's crash duty: whether the child that ended abnormally (`what`) is brought back,
/// by aterm's rule and back-off. It needs a claim (the link held, or lost and being claimed
/// again: without one the relaunch duty is aterm's own engine's). The ladder's pause comes
/// first (1 s, 10 s, 60 s, then 10 min, forgiven after 10 min of healthy running), and aterm's
/// word is asked fresh when the pause is over (`Link::relaunch_word`: the claim this harness's
/// and proven with `relaunch`, the policy in effect with `relaunch=true`, and nobody at the
/// keys within the grace). aterm away or answering "later" is waited for, asked again every
/// [`ASK_AGAIN`], within [`Timing::away_budget`] ([`AWAY_BUDGET`]) of time actually spent
/// waiting; a lost claim is waited for on its own reclaim clock meanwhile. An `INT`, `QUIT`,
/// `HUP` or `TERM` while it waits ends the session instead. A restart aterm asks for meanwhile
/// is passed over: the relaunch is pending.
///
/// # Errors
/// Why the crash is not relaunched.
#[allow(clippy::too_many_arguments)]
fn answer_crash(
    link: &mut Link,
    ladder: &mut Ladder,
    signals: &Signals,
    journal: &Journal,
    timing: &Timing,
    what: &str,
    session: &str,
    child: Child<'_>,
) -> Result<(), String> {
    if !link.claims() {
        return Err(
            "this harness holds no claim with relaunch scope (outside aterm, or aterm did not \
             take the claim)"
                .to_string(),
        );
    }
    let pause = ladder.crashed(aterm::now_ms());
    journal.line(
        "CRASH",
        &format!(
            "{what}; relaunch {} on session {session} in {} ms (aterm's back-off), on aterm's word",
            ladder.streak().saturating_add(1),
            pause.as_millis()
        ),
    );
    let due = Instant::now() + pause;
    let mut budget = AwayBudget::within(timing.away_budget);
    let mut next_ask = Instant::now();
    let mut away = String::new();
    loop {
        let now = Instant::now();
        if now >= next_ask {
            if link.is_lost() && budget.waiting() {
                // A lost link is claimed again on its own clock; asking now would only read it
                // lost again.
                next_ask = now + ASK_AGAIN;
            } else {
                let ms = aterm::now_ms();
                let word = link.relaunch_word(false, ms);
                aterm::journal(link, journal);
                match word {
                    Relaunch::Permitted => {
                        budget.answered(ms);
                        if now >= due {
                            return Ok(());
                        }
                        next_ask = due;
                    }
                    Relaunch::Denied(why) => return Err(format!("aterm: {why}")),
                    Relaunch::Away(why) | Relaunch::Later(why) => {
                        if budget.wait(ms) {
                            journal.line(
                                "CRASH-WAIT",
                                &format!(
                                    "aterm did not answer ({why}); asked again within the {} ms budget",
                                    timing.away_budget.as_millis()
                                ),
                            );
                        }
                        away = why;
                        next_ask = now + ASK_AGAIN;
                    }
                }
            }
        }
        if budget.spent(aterm::now_ms()) {
            return Err(format!("claim-not-proven-again {away}"));
        }
        let left = next_ask
            .saturating_duration_since(Instant::now())
            .min(timing.poll);
        if let Some(sig) = signals
            .wait_also(left, link.event_fd())
            .into_iter()
            .find(|sig| ends_a_crash(*sig))
        {
            return Err(format!(
                "signal {sig} came while it waited, so the session ends"
            ));
        }
        if let Some(event) = link_tick(link, child, journal) {
            journal.line(
                "RESTART-PASSED-OVER",
                &format!(
                    "{} grok crashed and its relaunch is pending",
                    restart_words(&event)
                ),
            );
        }
    }
}

/// The fault or abort a child ended with (`SEGV`, `BUS`, `ILL`, `ABRT`, `FPE`, `TRAP`,
/// `SYS`), or `None`: what reads, soon after a move, as the new build refusing to start
/// ([`refused_start`]). A crash relaunch is judged more widely, by aterm's rule
/// (`alab_wire::relaunch::abnormal_exit`).
#[must_use]
pub fn crash_signal(status: libc::c_int) -> Option<libc::c_int> {
    if !libc::WIFSIGNALED(status) {
        return None;
    }
    let sig = libc::WTERMSIG(status);
    [
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGABRT,
        libc::SIGFPE,
        libc::SIGTRAP,
        libc::SIGSYS,
    ]
    .contains(&sig)
    .then_some(sig)
}

/// The terminal's modes when the harness started, put back after a child ends (rustix's safe
/// `tcgetattr`/`tcsetattr`: libc's need a zeroed `termios` the verified lane refuses).
struct Modes(Option<Termios>);

impl Modes {
    fn take() -> Modes {
        Modes(tcgetattr(io::stdin()).ok())
    }

    /// Put the modes back, only while the harness's group owns the terminal (a background
    /// job never touches it).
    fn restore(&self) {
        let Some(t) = &self.0 else {
            return;
        };
        let stdin = io::stdin();
        let foreground = tcgetpgrp(&stdin).is_ok_and(|group| group == getpgrp());
        if foreground {
            let _ = tcsetattr(&stdin, OptionalActions::Now, t);
        }
    }
}

/// How one `waitpid` read the child.
enum Reaped {
    Running,
    Stopped,
    /// The child was continued: by the harness, or by a continue the harness saw no stop for
    /// (a whole-job stop that stopped the harness too).
    Continued,
    Ended(libc::c_int),
}

fn reap(pid: libc::pid_t) -> Reaped {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: `waitpid` on our own child, writing its status into the live local.
        let r = unsafe {
            libc::waitpid(
                pid,
                &raw mut status,
                libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
            )
        };
        if r == 0 {
            return Reaped::Running;
        }
        if r == pid {
            if libc::WIFSTOPPED(status) {
                return Reaped::Stopped;
            }
            if libc::WIFCONTINUED(status) {
                return Reaped::Continued;
            }
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                return Reaped::Ended(status);
            }
            return Reaped::Running;
        }
        if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        // No such child: it was reaped elsewhere, which this harness never does. End as a
        // failure rather than wait forever.
        return Reaped::Ended(exit_status(1));
    }
}

/// TIME STOPPED IS NOT GRACE (Decision 39): the grace a child has to answer the harness's own
/// SIGTERM starts again whenever the child is continued, so a child stopped past it is not
/// read as one that ignored the SIGTERM the moment it runs again. The clock is
/// [`Instant`], which stops in machine sleep on macOS (`CLOCK_UPTIME_RAW`) and Linux
/// (`CLOCK_MONOTONIC`), so a sleep is not grace either.
fn grace_again(moving: &mut Option<Moving>) {
    if let Some(m) = moving.as_mut() {
        m.sent = Instant::now();
    }
}

/// A wait status for exit code `code`.
#[must_use]
pub const fn exit_status(code: u8) -> libc::c_int {
    (code as libc::c_int) << 8
}

/// The first launch's argv: the injected session id (if any) in front of the launch's own.
#[must_use]
pub fn first_argv(plan: &Plan, args: &[OsString]) -> Vec<OsString> {
    let mut out: Vec<OsString> = match plan {
        Plan::Session(c) => c.injected.iter().map(OsString::from).collect(),
        _ => Vec::new(),
    };
    out.extend(args.iter().cloned());
    out
}

fn spawn(
    layout: &Layout,
    build: &Build,
    argv: &[OsString],
    ignored: &[libc::c_int],
    depth: u32,
) -> io::Result<libc::pid_t> {
    let mut cmd = Command::new(&build.bin);
    cmd.args(argv);
    for (name, value) in store::shim_env(layout, build) {
        cmd.env(name, value);
    }
    cmd.env(PID_ENV, std::process::id().to_string());
    cmd.env(DEPTH_ENV, depth.saturating_add(1).to_string());
    let ignored = ignored.to_vec();
    // SAFETY: the hook runs in the forked child before exec and only calls `signal`, which
    // is async-signal-safe, over a Vec allocated before the fork (iterating allocates
    // nothing).
    unsafe {
        cmd.pre_exec(move || {
            for sig in &ignored {
                libc::signal(*sig, libc::SIG_IGN);
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    libc::pid_t::try_from(child.id()).map_err(|_| io::Error::other("pid out of range"))
}

fn plan_words(plan: &Plan) -> String {
    match plan {
        Plan::Session(c) if c.injected.is_empty() => {
            format!("session {} (named by the launch); carried", c.session)
        }
        Plan::Session(c) => format!("session {} (started under this id); carried", c.session),
        Plan::Uncarried { why } => format!("interactive, not carried: {why}"),
        Plan::PassThrough { why, .. } => format!("not an interactive session: {why}"),
    }
}

/// Why a relaunch did not start.
enum Refusal {
    /// grok would start signed out ([`crate::credentials`]): a signed-out grok opens xAI's
    /// sign-in in the desk's browser, and a relaunch is the harness's act, not a person's.
    SignedOut,
    /// The kernel refused the spawn.
    Spawn(io::Error),
}

/// Whether `build`, started now by the harness, would start signed in: its shim environment
/// over the harness's own, read as Grok reads it ([`crate::credentials::signed_in`]).
fn signed_in_for(layout: &Layout, build: &Build) -> bool {
    let shim = store::shim_env(layout, build);
    crate::credentials::signed_in(&crate::credentials::child_env(&shim, &Env::process()))
}

/// Under aterm, sign a machine that `build` would start signed out on in through aterm's one
/// sign-in for the machine ([`crate::signin::decide`], always gated: a relaunch starts the
/// interactive screen), and say whether it is signed in now. Outside aterm (or with the harness
/// taken away) nothing is asked and the answer is no: a relaunch never opens a page itself.
fn signed_in_through_aterm(
    layout: &Layout,
    build: &Build,
    journal: &Journal,
    signin: &mut impl FnMut(&std::path::Path) -> Result<bool, String>,
) -> bool {
    let shim = store::shim_env(layout, build);
    let env = crate::credentials::child_env(&shim, &Env::process());
    if !crate::signin::applies(&env) {
        return false;
    }
    match crate::signin::decide(true, &env, signin) {
        crate::signin::Gate::Pass => true,
        crate::signin::Gate::SignedIn => {
            journal.line(
                "NOTE",
                "signed in to Grok Build through `aterm harness signin --cli grok` before the relaunch",
            );
            true
        }
        crate::signin::Gate::Refused(why) => {
            journal.line(
                "REFUSED",
                &format!("the sign-in before the relaunch: {why}"),
            );
            false
        }
    }
}

/// Start a relaunch of carried session `c` on `onto`: the harness's own act, so never onto a
/// grok that would start signed out. Under aterm a signed-out machine is signed in first
/// ([`signed_in_through_aterm`]); [`Refusal::SignedOut`] when it still is not.
fn relaunch(
    layout: &Layout,
    onto: &Build,
    c: &Carried,
    ignored: &[libc::c_int],
    depth: u32,
    journal: &Journal,
) -> Result<libc::pid_t, Refusal> {
    if !signed_in_for(layout, onto)
        && !signed_in_through_aterm(layout, onto, journal, &mut crate::signin::run_signin)
    {
        return Err(Refusal::SignedOut);
    }
    let argv: Vec<OsString> = c.relaunch_argv().into_iter().map(OsString::from).collect();
    spawn(layout, onto, &argv, ignored, depth).map_err(Refusal::Spawn)
}

/// The words a session that ended signed out leaves on the terminal and in the journal.
fn signed_out_words(what: &str, session: &str) -> String {
    format!(
        "{what}, but grok would start signed out (no GROK_AUTH, XAI_API_KEY or login file it accepts), and a \
         signed-out grok opens xAI's sign-in in a browser; it was not relaunched. Once signed in: \
         grok --resume {session}"
    )
}

/// How long after the harness's own unanswered SIGTERM an exit may still be that SIGTERM's
/// (and the move finished), when nobody typed since.
pub const LATE_EXIT_WINDOW: Duration = Duration::from_secs(60);

/// Whether `status` is what grok ends with on a SIGTERM: exit 0 (Grok Build 1.0.70 restores
/// the terminal and exits 0 on it, MEASURED) or death by `TERM`.
#[must_use]
pub fn ended_by_term(status: libc::c_int) -> bool {
    (libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0)
        || (libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGTERM)
}

/// Whether `status`, soon after a move started a build, is that build refusing to start: any
/// exit but 0 or a terminal/job signal's (`129` `HUP`, `130` `INT`, `131` `QUIT`, `143`
/// `TERM`), or a fault or an abort ([`crash_signal`]). A `KILL`, `TERM`, `HUP`, `INT` or
/// `QUIT` came from someone, never from the build.
#[must_use]
pub fn refused_start(status: libc::c_int) -> bool {
    (libc::WIFEXITED(status) && !matches!(libc::WEXITSTATUS(status), 0 | 129 | 130 | 131 | 143))
        || crash_signal(status).is_some()
}
/// What the harness's own unanswered signal came to.
enum Overdue {
    /// A move's SIGTERM went unanswered within the grace: the move is abandoned.
    Move,
    /// A restart's SIGKILL left grok running: the restart is abandoned.
    Restart,
}

/// Run the session to its end and answer the wait status to end like.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn run(s: Session<'_>) -> libc::c_int {
    let Session {
        layout,
        build,
        args,
        plan,
        journal,
        timing,
        signals,
        depth,
        mut link,
    } = s;
    let modes = Modes::take();
    let ignored = signals.inherited_ignored.clone();
    let argv = first_argv(&plan, &args);
    let mut child = match spawn(layout, &build, &argv, &ignored, depth) {
        Ok(pid) => pid,
        Err(e) => {
            eprintln!("grok-alab: cannot run {}: {e}", build.bin.display());
            journal.line(
                "REFUSED",
                &format!("cannot run {}: {e}", build.bin.display()),
            );
            let code = if e.kind() == io::ErrorKind::NotFound {
                127
            } else {
                126
            };
            return exit_status(code);
        }
    };
    let mut process = aterm::process_of(child);
    journal.line(
        "START",
        &format!(
            "{} {} runs grok {} (build {}) as pid {child}; {}; {}",
            crate::NAME,
            crate::VERSION,
            build.version_label(),
            build.id,
            plan_words(&plan),
            supervision(&link)
        ),
    );
    let mut deferred: Option<Deferred> = None;
    announce(
        &mut link,
        claim_child(&build, process),
        &mut deferred,
        journal,
    );
    let mut carried: Option<Carried> = match &plan {
        Plan::Session(c) => Some(c.clone()),
        _ => None,
    };
    let uncarried = match &plan {
        Plan::Session(_) => None,
        Plan::Uncarried { why } => Some(why.clone()),
        Plan::PassThrough { why, .. } => Some(format!("not an interactive session ({why})")),
    };
    let mut running = build;
    let mut ending = false;
    // The harness's own SIGTERM for a move or a restart aterm asked for, under way.
    let mut moving: Option<Moving> = None;
    let mut moves = 0_u32;
    // A move's SIGTERM grok did not answer within the grace: when it was sent, and the target.
    let mut unanswered_term: Option<(Instant, Build)> = None;
    // The last move's relaunch: when, and the build it moved from (a refused start goes back).
    let mut moved: Option<(Instant, Build)> = None;
    // Builds a move onto refused to start: never moved onto again in this session.
    let mut failed: BTreeSet<u64> = BTreeSet::new();
    let mut seen: Option<u64> = None;
    let mut seen_self: Option<u64> = None;
    let mut started = Instant::now();
    let mut ladder = match timing.ladder {
        Some((streak, since)) => {
            journal.line(
                "NOTE",
                &format!(
                    "this test build's crash ladder starts at streak {streak}, its last relaunch {} ms ago",
                    since.as_millis()
                ),
            );
            Ladder::carried(streak, Some(since), aterm::now_ms())
        }
        None => Ladder::new(),
    };
    let mut waiting: Option<String> = None;
    loop {
        // Stat the terminal once per tick, BEFORE waiting: macOS stamps a terminal's times
        // lazily (a read or write soon after its last stamp is stamped at the NEXT stat, with
        // that stat's time; MEASURED, [`procs::tty_quiet`]), so without this the stat a
        // request makes would stamp output from long ago as "0 ms ago". Stat here, each event
        // is stamped at most one poll after it happened, and a request reads true ages (or
        // younger ones, never older).
        let _ = procs::tty_quiet(io::stdin().as_fd(), SystemTime::now());
        let mut request = false;
        for sig in signals.wait_also(timing.poll, link.event_fd()) {
            match sig {
                libc::SIGHUP | libc::SIGTERM => {
                    ending = true;
                    let name = if sig == libc::SIGHUP { "HUP" } else { "TERM" };
                    let sent = signals::send(child, sig);
                    journal.line(
                        "PASS",
                        &format!(
                            "{name} passed on to grok (sent={sent}); the session ends with it"
                        ),
                    );
                }
                libc::SIGCONT => {
                    let _ = signals::send(child, libc::SIGCONT);
                    grace_again(&mut moving);
                }
                libc::SIGUSR1 => request = true,
                // INT and QUIT are the child's: the terminal gave it its own copy. TSTP is
                // followed once the child stops; CHLD only wakes the loop.
                _ => {}
            }
        }
        loop {
            match reap(child) {
                Reaped::Running => break,
                Reaped::Stopped => {
                    journal.line("STOP", "grok stopped; the harness follows it");
                    signals::stop_self();
                    let _ = signals::send(child, libc::SIGCONT);
                    grace_again(&mut moving);
                    journal.line("CONT", "continued; grok continued with it");
                }
                Reaped::Continued => grace_again(&mut moving),
                Reaped::Ended(status) => {
                    // A crashed child's raw mode never leaks into the next child or the shell.
                    modes.restore();
                    let ran = started.elapsed();
                    let ours = moving.take().filter(|m| {
                        !ending && (m.asked.is_some() || m.sent.elapsed() <= timing.exit_grace)
                    });
                    let late = unanswered_term.take();
                    let just_moved = moved
                        .take()
                        .filter(|(at, _)| !ending && at.elapsed() < timing.min_run);
                    // 0. A restart aterm asked for: grok ended on the harness's signal, and the
                    //    same session resumes on the running build, on the model aterm named.
                    let (ours, asked) = match ours {
                        Some(Moving {
                            asked: Some(asked),
                            target,
                            ..
                        }) => (None, Some((asked, target))),
                        other => (other, None),
                    };
                    if let Some((asked, target)) = asked {
                        let onto = store::build(layout, target.id)
                            .ok()
                            .filter(|b| !crate::is_self(&b.bin))
                            .unwrap_or(target);
                        let c = &asked.carried;
                        match relaunch(layout, &onto, c, &ignored, depth, journal) {
                            Ok(pid) => {
                                journal.line(
                                    "RELAUNCHED",
                                    &format!(
                                        "grok {} as pid {pid} for aterm's restart {}: {}",
                                        onto.version_label(),
                                        asked.words,
                                        c.relaunch_argv().join(" ")
                                    ),
                                );
                                child = pid;
                                process = aterm::process_of(pid);
                                running = onto;
                                started = Instant::now();
                                waiting = None;
                                carried = Some(asked.carried);
                                announce(
                                    &mut link,
                                    claim_child(&running, process),
                                    &mut deferred,
                                    journal,
                                );
                                continue;
                            }
                            Err(Refusal::SignedOut) => {
                                let why = signed_out_words(
                                    &format!(
                                        "grok ended for aterm's restart {} ({})",
                                        asked.words,
                                        describe(status)
                                    ),
                                    &c.session,
                                );
                                journal.line("EXIT", &why);
                                eprintln!("grok-alab: {why}");
                                return status;
                            }
                            Err(Refusal::Spawn(e)) => {
                                let why = format!(
                                    "grok could not be started again for aterm's restart ({e}); resume it with: grok --resume {}",
                                    c.session
                                );
                                journal.line("EXIT", &why);
                                eprintln!("grok-alab: {why}");
                                return exit_status(127);
                            }
                        }
                    }
                    // 1. The move's own exit, or (2) a late one after its unanswered SIGTERM
                    //    with nobody typing since: finish the move, on the move's target.
                    let mut late_not_ours: Option<String> = None;
                    let finish = match (ours, &late) {
                        (Some(m), _) => Some((m.target, "")),
                        (None, Some((sent, target))) if !ending => {
                            let after = sent.elapsed();
                            if !ended_by_term(status) {
                                late_not_ours = Some(format!(
                                    "it ended ({}), which is not grok's answer to a SIGTERM",
                                    describe(status)
                                ));
                                None
                            } else if after > LATE_EXIT_WINDOW {
                                late_not_ours = Some(format!(
                                    "{} s after it, past the {} s a late answer is read as one",
                                    after.as_secs(),
                                    LATE_EXIT_WINDOW.as_secs()
                                ));
                                None
                            } else if let Some(why) = typed_since(&mut link, after, journal) {
                                late_not_ours = Some(format!("{why}, so it reads as their quit"));
                                None
                            } else {
                                Some((target.clone(), " after a late exit"))
                            }
                        }
                        _ => None,
                    };
                    if let (Some((target, how)), Some(c)) = (finish, carried.as_ref()) {
                        // A standing claim that no longer takes `upgrade` (a person's cap
                        // withdrew it after the move's SIGTERM) leaves moves to aterm's engine:
                        // the session resumes on the build it ran (Decision 52).
                        let withheld = link.grant().is_some_and(|g| !g.takes(Scope::Upgrade));
                        let onto = if withheld {
                            journal.line(
                                "HOLD",
                                &format!(
                                    "aterm no longer grants upgrade (scope={}); grok resumes on {}, not {}",
                                    aterm::granted(&link),
                                    running.version_label(),
                                    target.version_label()
                                ),
                            );
                            startable(layout, running.id).unwrap_or_else(|_| running.clone())
                        } else {
                            if target.id != running.id && startable(layout, target.id).is_err() {
                                failed.insert(target.id);
                            }
                            relaunch_target(layout, &running, &target, &failed)
                        };
                        let attempt = match relaunch(layout, &onto, c, &ignored, depth, journal) {
                            // The new build would not start: resume on the build it ran.
                            Err(Refusal::Spawn(e)) if onto.id != running.id => {
                                journal.line(
                                    "FAILED",
                                    &format!(
                                        "grok {} could not be started ({e}); back on {}",
                                        onto.version_label(),
                                        running.version_label()
                                    ),
                                );
                                failed.insert(onto.id);
                                let back = store::build(layout, running.id)
                                    .ok()
                                    .filter(|b| !crate::is_self(&b.bin))
                                    .unwrap_or_else(|| running.clone());
                                relaunch(layout, &back, c, &ignored, depth, journal)
                                    .map(|pid| (pid, back))
                            }
                            other => other.map(|pid| (pid, onto)),
                        };
                        match attempt {
                            Ok((pid, onto)) => {
                                journal.line(
                                    "RELAUNCHED",
                                    &format!(
                                        "grok {} -> {} as pid {pid}{how}: {}",
                                        running.version_label(),
                                        onto.version_label(),
                                        c.relaunch_argv().join(" ")
                                    ),
                                );
                                if onto.id != running.id {
                                    moved = Some((Instant::now(), running.clone()));
                                    moves = moves.saturating_add(1);
                                }
                                child = pid;
                                process = aterm::process_of(pid);
                                running = onto;
                                started = Instant::now();
                                waiting = None;
                                announce(
                                    &mut link,
                                    claim_child(&running, process),
                                    &mut deferred,
                                    journal,
                                );
                                continue;
                            }
                            Err(Refusal::SignedOut) => {
                                let why = signed_out_words(
                                    &format!("grok ended for a live update ({})", describe(status)),
                                    &c.session,
                                );
                                journal.line("EXIT", &why);
                                eprintln!("grok-alab: {why}");
                                return status;
                            }
                            Err(Refusal::Spawn(e)) => {
                                let why = format!(
                                    "grok could not be started again ({e}); resume it with: grok --resume {}",
                                    c.session
                                );
                                journal.line("EXIT", &why);
                                eprintln!("grok-alab: {why}");
                                return exit_status(127);
                            }
                        }
                    }
                    // 3. The build a move just started refused to start: back on the build it
                    //    moved from (aterm's harness design §4.3 step 6).
                    if let (Some((_, prev)), Some(c)) = (just_moved, carried.as_ref())
                        && refused_start(status)
                    {
                        failed.insert(running.id);
                        let back = store::build(layout, prev.id)
                            .ok()
                            .filter(|b| !crate::is_self(&b.bin));
                        let what = format!(
                            "the move onto grok {} failed: it ended ({}) {} ms after its start",
                            running.version_label(),
                            describe(status),
                            ran.as_millis()
                        );
                        let Some(back) = back else {
                            let why = format!(
                                "{what}, and grok {} is no longer in the store; resume with: grok --resume {}",
                                prev.version_label(),
                                c.session
                            );
                            journal.line("EXIT", &why);
                            eprintln!("grok-alab: {why}");
                            return status;
                        };
                        match relaunch(layout, &back, c, &ignored, depth, journal) {
                            Ok(pid) => {
                                journal.line(
                                    "FAILED",
                                    &format!(
                                        "{what}; relaunched on grok {} as pid {pid}: {}",
                                        back.version_label(),
                                        c.relaunch_argv().join(" ")
                                    ),
                                );
                                child = pid;
                                process = aterm::process_of(pid);
                                running = back;
                                started = Instant::now();
                                waiting = None;
                                announce(
                                    &mut link,
                                    claim_child(&running, process),
                                    &mut deferred,
                                    journal,
                                );
                                continue;
                            }
                            Err(Refusal::SignedOut) => {
                                let why = signed_out_words(&what, &c.session);
                                journal.line("EXIT", &why);
                                eprintln!("grok-alab: {why}");
                                return status;
                            }
                            Err(Refusal::Spawn(e)) => {
                                let why = format!(
                                    "{what}, and grok {} could not be started ({e}); resume with: grok --resume {}",
                                    back.version_label(),
                                    c.session
                                );
                                journal.line("EXIT", &why);
                                eprintln!("grok-alab: {why}");
                                return status;
                            }
                        }
                    }
                    // 4. An abnormal end under a claim: aterm's rule, word and back-off.
                    if let Some(c) = carried.clone()
                        && !ending
                        && alab_wire::relaunch::abnormal_exit(child_end(status))
                    {
                        let what = format!(
                            "grok {} crashed ({}) after {} ms",
                            running.version_label(),
                            describe(status),
                            ran.as_millis()
                        );
                        match answer_crash(
                            &mut link,
                            &mut ladder,
                            signals,
                            journal,
                            &timing,
                            &what,
                            &c.session,
                            claim_child(&running, None),
                        ) {
                            Err(why) => {
                                journal.line("EXIT", &format!("{what}; not relaunched: {why}"));
                            }
                            Ok(()) => {
                                // A crash relaunch takes the newest ready build only while the
                                // claim holds `upgrade`: a move onto it is that duty's.
                                let onto = if link.takes(Scope::Upgrade) {
                                    relaunch_target(layout, &running, &running, &failed)
                                } else {
                                    store::build(layout, running.id)
                                        .ok()
                                        .filter(|b| !crate::is_self(&b.bin))
                                        .unwrap_or_else(|| running.clone())
                                };
                                let attempt =
                                    match relaunch(layout, &onto, &c, &ignored, depth, journal) {
                                        Err(Refusal::Spawn(e)) if onto.id != running.id => {
                                            journal.line(
                                            "FAILED",
                                            &format!(
                                                "grok {} could not be started ({e}); back on {}",
                                                onto.version_label(),
                                                running.version_label()
                                            ),
                                        );
                                            failed.insert(onto.id);
                                            let back = store::build(layout, running.id)
                                                .ok()
                                                .filter(|b| !crate::is_self(&b.bin))
                                                .unwrap_or_else(|| running.clone());
                                            relaunch(layout, &back, &c, &ignored, depth, journal)
                                                .map(|pid| (pid, back))
                                        }
                                        other => other.map(|pid| (pid, onto)),
                                    };
                                match attempt {
                                    Ok((pid, onto)) => {
                                        ladder.relaunched(aterm::now_ms());
                                        journal.line(
                                            "RELAUNCHED",
                                            &format!(
                                                "after a crash: grok {} as pid {pid}: {}",
                                                onto.version_label(),
                                                c.relaunch_argv().join(" ")
                                            ),
                                        );
                                        if onto.id != running.id {
                                            moved = Some((Instant::now(), running.clone()));
                                            moves = moves.saturating_add(1);
                                        }
                                        child = pid;
                                        process = aterm::process_of(pid);
                                        running = onto;
                                        started = Instant::now();
                                        waiting = None;
                                        announce(
                                            &mut link,
                                            claim_child(&running, process),
                                            &mut deferred,
                                            journal,
                                        );
                                        continue;
                                    }
                                    Err(Refusal::SignedOut) => {
                                        let why = signed_out_words(
                                            &format!("grok crashed ({})", describe(status)),
                                            &c.session,
                                        );
                                        journal.line("EXIT", &why);
                                        eprintln!("grok-alab: {why}");
                                        return status;
                                    }
                                    Err(Refusal::Spawn(e)) => {
                                        journal.line(
                                            "EXIT",
                                            &format!("the crash relaunch failed: {e}"),
                                        );
                                        eprintln!(
                                            "grok-alab: could not relaunch grok {} after a crash ({e}); resume it with: grok --resume {}",
                                            running.version_label(),
                                            c.session
                                        );
                                        return status;
                                    }
                                }
                            }
                        }
                    }
                    // 5. The session's end.
                    journal.line("EXIT", &format!("grok ended: {}", describe(status)));
                    let clean = libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0;
                    if let Some(c) = carried.as_ref() {
                        if let Some(why) = late_not_ours {
                            // The harness's SIGTERM cannot be taken back: an exit after it that
                            // could not be read as its answer ends the session here, clean or
                            // not. Never let that pass in silence.
                            let why = format!(
                                "grok {} ended ({}) after the harness's unanswered SIGTERM for a live update; not relaunched: {why}. Resume it with: grok --resume {}",
                                running.version_label(),
                                describe(status),
                                c.session
                            );
                            journal.line("NOTE", &why);
                            eprintln!("grok-alab: {why}");
                        } else if !clean && moves > 0 {
                            // No exit table is measured: an exit by code cannot be told from a
                            // person's, so it ends the session, and the id the harness started
                            // it under is said once.
                            eprintln!(
                                "grok-alab: grok {} ended ({}) after a live update; resume it with: grok --resume {}",
                                running.version_label(),
                                describe(status),
                                c.session
                            );
                        }
                    }
                    return status;
                }
            }
        }
        // The harness's own SIGTERM, unanswered: a move is abandoned (never killed harder); a
        // restart aterm asked for is made with SIGKILL, and abandoned only if even that leaves
        // grok running.
        let mut overdue: Option<Overdue> = None;
        if let Some(m) = moving.as_mut() {
            let elapsed = m.sent.elapsed();
            match m.asked.as_mut() {
                None if elapsed > timing.exit_grace => {
                    journal.line(
                        "HOLD",
                        &format!(
                            "grok did not exit within {} ms of the harness's SIGTERM; it runs on, on {} (never killed harder)",
                            timing.exit_grace.as_millis(),
                            running.version_label()
                        ),
                    );
                    overdue = Some(Overdue::Move);
                }
                Some(asked) if !asked.killed && elapsed > asked.kill_after => {
                    let sent = signals::send(child, libc::SIGKILL);
                    journal.line(
                        "RESTART",
                        &format!(
                            "{}: grok did not end within {} ms of the SIGTERM; SIGKILL (sent={sent})",
                            asked.words,
                            asked.kill_after.as_millis()
                        ),
                    );
                    asked.killed = true;
                    m.sent = Instant::now();
                }
                Some(asked) if asked.killed && elapsed > timing.exit_grace => {
                    journal.line(
                        "RESTART-ABANDONED",
                        &format!(
                            "{}: grok still runs {} ms after its SIGKILL",
                            asked.words,
                            timing.exit_grace.as_millis()
                        ),
                    );
                    overdue = Some(Overdue::Restart);
                }
                _ => {}
            }
        }
        match overdue {
            None => {}
            Some(Overdue::Move) => unanswered_term = moving.take().map(|m| (m.sent, m.target)),
            Some(Overdue::Restart) => moving = None,
        }
        // aterm's link: claimed again when lost, the policy followed, and a restart aterm asks
        // for taken up (one at a time, never while a move or another restart is under way).
        if let Some(event) = link_tick(&mut link, claim_child(&running, process), journal) {
            let busy = if ending {
                Some("the session is ending")
            } else if moving.is_some() {
                Some("a move or a restart is under way")
            } else {
                None
            };
            if let Some(why) = busy {
                journal.line(
                    "RESTART-PASSED-OVER",
                    &format!("{} {why}", restart_words(&event)),
                );
            } else {
                if let Some(earlier) = deferred.take() {
                    journal.line(
                        "RESTART-PASSED-OVER",
                        &format!(
                            "{} a later restart came in its place",
                            restart_words(&earlier.event)
                        ),
                    );
                }
                let now = Instant::now();
                deferred = Some(Deferred {
                    event,
                    expires: now + timing.restart_wait,
                    next_ask: now,
                    why: String::new(),
                });
            }
        }
        if let Some(mut waiting_restart) = deferred.take() {
            let now = Instant::now();
            if ending || moving.is_some() {
                journal.line(
                    "RESTART-PASSED-OVER",
                    &format!(
                        "{} the session is ending or a move is under way",
                        restart_words(&waiting_restart.event)
                    ),
                );
            } else if now >= waiting_restart.expires {
                journal.line(
                    "RESTART-REFUSED",
                    &format!(
                        "{} not made within {} ms: {}",
                        restart_words(&waiting_restart.event),
                        timing.restart_wait.as_millis(),
                        waiting_restart.why
                    ),
                );
            } else if now < waiting_restart.next_ask {
                deferred = Some(waiting_restart);
            } else {
                match restart_now(
                    &mut link,
                    &waiting_restart.event,
                    layout,
                    &running,
                    carried.as_ref(),
                    child,
                    &timing,
                    journal,
                ) {
                    Answer::Moving(m) => {
                        moving = Some(m);
                        unanswered_term = None;
                    }
                    Answer::Refused => {}
                    Answer::Waits(why) => {
                        if why != waiting_restart.why {
                            journal.line(
                                "RESTART-WAIT",
                                &format!(
                                    "{} {why}; asked again within {} ms of its arrival",
                                    restart_words(&waiting_restart.event),
                                    timing.restart_wait.as_millis()
                                ),
                            );
                            waiting_restart.why = why;
                        }
                        waiting_restart.next_ask = now + ASK_AGAIN;
                        deferred = Some(waiting_restart);
                    }
                }
            }
        }
        let newer = match store::current(layout)
            .ok()
            .filter(|b| b.id > running.id && !failed.contains(&b.id))
        {
            // A newer build that is this very harness is never moved onto: the harness never
            // execs itself (the first launch's check, [`crate::is_self`], holds for every
            // relaunch too).
            Some(b) if crate::is_self(&b.bin) => {
                if seen_self != Some(b.id) {
                    seen_self = Some(b.id);
                    journal.line(
                        "REFUSED",
                        &format!(
                            "build {} ({}) is this harness itself; never moved onto",
                            b.id,
                            b.bin.display()
                        ),
                    );
                }
                None
            }
            other => other,
        };
        if let Some(b) = &newer
            && seen != Some(b.id)
        {
            seen = Some(b.id);
            journal.line(
                "SEEN",
                &format!(
                    "grok {} (build {}) is installed beside the running {}; it moves at a safe point ({})",
                    b.version_label(),
                    b.id,
                    running.version_label(),
                    if link.judges() && link.takes(Scope::Upgrade) {
                        "aterm's read, or SIGUSR1"
                    } else {
                        "SIGUSR1"
                    }
                ),
            );
        }
        // Under a standing claim, aterm's read is the witness on every tick a newer build
        // waits; a request is the person's override, on aterm's word too.
        let witness = if request {
            Some(Witness::Request(person_word(&mut link, journal)))
        } else if newer.is_some()
            && moving.is_none()
            && unanswered_term
                .as_ref()
                .is_none_or(|(_, t)| Some(t.id) != newer.as_ref().map(|b| b.id))
            && !ending
            && uncarried.is_none()
            && link.judges()
        {
            Some(aterm_witness(&mut link, journal))
        } else {
            None
        };
        if let Some(witness) = witness {
            let asked = matches!(witness, Witness::Request(_));
            let facts = Facts {
                ending,
                stopped: false,
                moving: moving.is_some(),
                uncarried: uncarried.clone(),
                newer: newer.as_ref().map(Build::version_label),
                signed_in: newer.as_ref().is_none_or(|b| signed_in_for(layout, b)),
                descendants: procs::descendants(child).map(|d| d.len()),
                tty: procs::tty_quiet(io::stdin().as_fd(), SystemTime::now()),
                quiet: timing.quiet,
                human_grace: timing.human_grace,
                witness,
            };
            match (safepoint::decide(&facts), newer) {
                (Decision::Move, Some(target)) => {
                    if signals::send(child, libc::SIGTERM) {
                        journal.line(
                            "MOVE",
                            &format!(
                                "safe point: SIGTERM to grok {} to relaunch it on {} (witness: {})",
                                running.version_label(),
                                target.version_label(),
                                if asked { "SIGUSR1" } else { "aterm's read" }
                            ),
                        );
                        moving = Some(Moving {
                            sent: Instant::now(),
                            target,
                            asked: None,
                        });
                        unanswered_term = None;
                        if let Some(waiting_restart) = deferred.take() {
                            journal.line(
                                "RESTART-PASSED-OVER",
                                &format!(
                                    "{} a move is under way",
                                    restart_words(&waiting_restart.event)
                                ),
                            );
                        }
                    } else {
                        journal.line("HOLD", "the kernel refused the SIGTERM; grok runs on");
                    }
                }
                (Decision::Move, None) => {
                    journal.line("HOLD", "no newer grok build is installed");
                }
                (Decision::Hold(why), _) if asked => {
                    journal.line("HOLD", &format!("safe point refused: {why}"));
                }
                (Decision::Hold(why), _) => {
                    // aterm is read again next tick: say each new reason once.
                    if waiting.as_deref() != Some(why.as_str()) {
                        journal.line("WAIT", &format!("not yet a safe point: {why}"));
                        waiting = Some(why);
                    }
                }
            }
        }
    }
}

/// The build a move relaunches on: the store's current build when it is still newer than
/// the running one and ready, else the move's target when it is still ready, else the
/// running build itself (the session is resumed either way, never dropped). A build that is
/// this harness itself ([`crate::is_self`]) or already failed in this session is never chosen.
fn relaunch_target(
    layout: &Layout,
    running: &Build,
    target: &Build,
    failed: &BTreeSet<u64>,
) -> Build {
    let usable = |b: &Build| !crate::is_self(&b.bin) && !failed.contains(&b.id);
    if let Ok(b) = store::current(layout)
        && b.id > running.id
        && usable(&b)
    {
        return b;
    }
    if let Ok(b) = store::build(layout, target.id)
        && usable(&b)
    {
        return b;
    }
    store::build(layout, running.id)
        .ok()
        .filter(|b| !crate::is_self(&b.bin))
        .unwrap_or_else(|| running.clone())
}

/// A wait status in words.
#[must_use]
pub fn describe(status: libc::c_int) -> String {
    if libc::WIFEXITED(status) {
        format!("exit {}", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        format!("signal {}", libc::WTERMSIG(status))
    } else {
        format!("status {status}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_argv_puts_the_injected_id_in_front_of_the_launch() {
        let plan = Plan::Session(Carried {
            session: "x".to_string(),
            kept: vec![],
            injected: vec!["--session-id".to_string(), "x".to_string()],
        });
        let args = vec![
            OsString::from("--model"),
            OsString::from("m"),
            OsString::from("hi"),
        ];
        assert_eq!(
            first_argv(&plan, &args),
            ["--session-id", "x", "--model", "m", "hi"].map(OsString::from)
        );
        let plan = Plan::PassThrough {
            why: String::new(),
            command: Some("sessions".to_string()),
        };
        assert_eq!(first_argv(&plan, &args), args);
    }

    #[test]
    fn timing_overrides_must_be_in_range() {
        let env = Env::from_pairs([
            (POLL_ENV, "50"),
            (QUIET_ENV, "0"),
            (HUMAN_GRACE_ENV, "nope"),
            (EXIT_GRACE_ENV, "5"),
        ]);
        let t = Timing::from_env(&env, &Journal::none());
        assert_eq!(t.poll, Duration::from_millis(50));
        assert_eq!(t.quiet, Duration::ZERO);
        assert_eq!(t.human_grace, safepoint::DEFAULT_HUMAN_GRACE);
        assert_eq!(t.exit_grace, Duration::from_secs(10));
        assert_eq!(
            Timing::from_env(&Env::default(), &Journal::none()),
            Timing::default()
        );
        // A restart aterm asked for waits for aterm at most its away budget by default.
        assert_eq!(Timing::default().restart_wait, AWAY_BUDGET);
        for (raw, want) in [
            ("1500", Duration::from_millis(1500)),
            ("3600000", Duration::from_secs(3600)),
            ("99", AWAY_BUDGET),
            ("3600001", AWAY_BUDGET),
        ] {
            let t = Timing::from_env(
                &Env::from_pairs([(RESTART_WAIT_ENV, raw)]),
                &Journal::none(),
            );
            assert_eq!(t.restart_wait, want, "{raw}");
        }
    }

    /// A test build (every debug build) reads the test knobs: a shorter away budget, never a
    /// longer one than aterm's, and the crash ladder to start from. Without them the budget is
    /// aterm's and the ladder empty.
    #[cfg(debug_assertions)]
    #[test]
    fn a_test_build_reads_the_away_budget_and_the_ladder_to_start_from() {
        let t = Timing::from_env(
            &Env::from_pairs([
                (TEST_AWAY_BUDGET_ENV, "1500"),
                (TEST_LADDER_ENV, "3,598500"),
            ]),
            &Journal::none(),
        );
        assert_eq!(t.away_budget, Duration::from_millis(1500));
        assert_eq!(t.ladder, Some((3, Duration::from_millis(598_500))));
        let none = Timing::from_env(&Env::default(), &Journal::none());
        assert_eq!((none.away_budget, none.ladder), (AWAY_BUDGET, None));
        for raw in ["99", "45001", "soon"] {
            let t = Timing::from_env(
                &Env::from_pairs([(TEST_AWAY_BUDGET_ENV, raw)]),
                &Journal::none(),
            );
            assert_eq!(t.away_budget, AWAY_BUDGET, "{raw}");
        }
        assert_eq!(test_ladder(" 2 , 0 "), Some((2, Duration::ZERO)));
        for raw in ["3", "a,1", "3,", ",1", "3,-1", "-1,0"] {
            assert_eq!(test_ladder(raw), None, "{raw}");
            let t = Timing::from_env(&Env::from_pairs([(TEST_LADDER_ENV, raw)]), &Journal::none());
            assert_eq!(t.ladder, None, "{raw}");
        }
    }

    /// aterm's rule reads a wait status: a code or a signal is the end, and the signals a
    /// person or the session ends a program with end a crash's wait.
    #[test]
    fn a_wait_status_is_judged_as_aterm_judges_a_childs_end() {
        assert_eq!(child_end(exit_status(3)), ChildEnd::Exited(3));
        assert_eq!(child_end(libc::SIGSEGV), ChildEnd::Signaled(libc::SIGSEGV));
        for (status, abnormal) in [
            (exit_status(0), false),
            (exit_status(1), true),
            (exit_status(130), false),
            (libc::SIGSEGV, true),
            (libc::SIGKILL, true),
            (libc::SIGTERM, false),
            (libc::SIGHUP, false),
        ] {
            assert_eq!(
                alab_wire::relaunch::abnormal_exit(child_end(status)),
                abnormal,
                "{}",
                describe(status)
            );
        }
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
            assert!(ends_a_crash(sig), "{sig}");
        }
        for sig in [libc::SIGUSR1, libc::SIGCONT, libc::SIGTSTP, libc::SIGCHLD] {
            assert!(!ends_a_crash(sig), "{sig}");
        }
    }

    /// A restart's journal words name its cause, model and account, each made safe for one
    /// line.
    #[test]
    fn a_restart_reads_in_journal_words() {
        let restart = |cause: &str, model: Option<&str>| Restart {
            cause: cause.to_string(),
            account: None,
            model: model.map(str::to_string),
        };
        assert_eq!(restart_words(&restart("asked", None)), "cause=asked");
        assert_eq!(
            restart_words(&restart("model", Some("grok-4"))),
            "cause=model model=grok-4"
        );
        assert_eq!(
            restart_words(&restart("a b", None)),
            "cause=a%20b",
            "one journal word"
        );
        let moved = Restart {
            account: Some(alab_wire::Account {
                label: "work".to_string(),
                home: None,
            }),
            ..restart("account", None)
        };
        assert_eq!(restart_words(&moved), "cause=account to=work");
    }

    #[test]
    fn only_a_fault_or_an_abort_is_a_crash() {
        for sig in [
            libc::SIGSEGV,
            libc::SIGBUS,
            libc::SIGILL,
            libc::SIGABRT,
            libc::SIGFPE,
        ] {
            assert_eq!(crash_signal(sig), Some(sig), "{sig}");
        }
        for sig in [
            libc::SIGKILL,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
        ] {
            assert_eq!(crash_signal(sig), None, "{sig}");
        }
        assert_eq!(crash_signal(exit_status(1)), None);
        assert_eq!(
            crash_signal(exit_status(139)),
            None,
            "an exit code is never a signal"
        );
    }

    #[test]
    fn refused_start_excludes_clean_and_user_signal_exits() {
        assert!(!refused_start(exit_status(0)));
        for code in [129, 130, 131, 143] {
            assert!(!refused_start(exit_status(code)), "{code}");
        }
        for code in [1, 2, 126, 127] {
            assert!(refused_start(exit_status(code)), "{code}");
        }
        for sig in [
            libc::SIGKILL,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGINT,
            libc::SIGQUIT,
        ] {
            assert!(!refused_start(sig), "{sig}");
        }
        for sig in [libc::SIGABRT, libc::SIGILL, libc::SIGSEGV] {
            assert!(refused_start(sig), "{sig}");
        }
    }

    #[test]
    fn a_wait_status_reads_in_words() {
        assert_eq!(describe(exit_status(3)), "exit 3");
        assert_eq!(describe(libc::SIGKILL), "signal 9");
    }
}
