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
//! Under a proven aterm claim ([`crate::aterm::Claimed`], scope `relaunch,upgrade`) the loop
//! also reads aterm for the safe point on every tick a newer build waits ([`safepoint::Witness`]),
//! re-announces the claim's `vendor=` after a move, watches the claim's connection on every tick
//! (made again at once when aterm closed it), follows the policy on its `harness-policy` event
//! (the claim given back while it is off), and relaunches a child that crashed
//! ([`crash_signal`]) on the same session after a fresh read of aterm.
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

use rustix::process::getpgrp;
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetpgrp, tcsetattr};

use crate::aterm::{self, Claimed, Locator};
use crate::env::Env;
use crate::journal::Journal;
use crate::lanes::{Carried, Plan};
use crate::procs;
use crate::safepoint::{self, AtermRead, Decision, Facts, Witness};
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
pub const CRASH_PAUSE_ENV: &str = "GROK_ALAB_CRASH_PAUSE_MS";

/// How many crashes one harness relaunches before it lets the next one end the session.
pub const MAX_CRASH_RELAUNCHES: u32 = 3;

/// How often a lost claim is made again.
pub const RECLAIM_EVERY: Duration = Duration::from_secs(5);

/// The loop's timings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How often the store's `current` link is read.
    pub poll: Duration,
    /// The output quiet a move needs ([`safepoint::DEFAULT_QUIET`]).
    pub quiet: Duration,
    /// The input quiet a move needs ([`safepoint::DEFAULT_HUMAN_GRACE`]).
    pub human_grace: Duration,
    /// How long the child has to exit after the harness's own SIGTERM before the move is
    /// abandoned (the child is never killed harder).
    pub exit_grace: Duration,
    /// How long a child must have run for its crash to be relaunched (a shorter run is a
    /// refused start, which is never looped on).
    pub min_run: Duration,
    /// The pause before the first crash relaunch; each next one doubles it.
    pub crash_pause: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            poll: Duration::from_secs(1),
            quiet: safepoint::DEFAULT_QUIET,
            human_grace: safepoint::DEFAULT_HUMAN_GRACE,
            exit_grace: Duration::from_secs(10),
            min_run: Duration::from_secs(10),
            crash_pause: Duration::from_secs(1),
        }
    }
}

impl Timing {
    /// The defaults with the overrides `env` sets; a value that is not a number in range is
    /// ignored (and journaled).
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
        Timing {
            poll: ms(POLL_ENV, 20, 60_000, d.poll),
            quiet: ms(QUIET_ENV, 0, 86_400_000, d.quiet),
            human_grace: ms(HUMAN_GRACE_ENV, 0, 86_400_000, d.human_grace),
            exit_grace: ms(EXIT_GRACE_ENV, 100, 600_000, d.exit_grace),
            min_run: ms(MIN_RUN_ENV, 0, 600_000, d.min_run),
            crash_pause: ms(CRASH_PAUSE_ENV, 0, 60_000, d.crash_pause),
        }
    }
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
    /// The proven aterm claim, or `None` outside aterm.
    pub claim: Option<Box<Claimed>>,
}

/// Where the harness stands with aterm while it runs.
#[derive(Debug)]
enum Link {
    /// Not under aterm: a request is the only witness.
    Off,
    /// A proven claim.
    Held(Standing),
    /// aterm's harness policy went off for the session (Autopilot Off, `[harness] enabled =
    /// false`, contract 3 no longer served): the claim was given back on its connection, which
    /// stays open with the subscription, and is made again once the policy is back on.
    Released(Standing),
    /// The claim's connection failed; it is made again at once, then every [`RECLAIM_EVERY`].
    Lost {
        locator: Locator,
        tried: Option<Instant>,
        why: Option<String>,
    },
}

/// A claim's connection and when its policy was last read.
#[derive(Debug)]
struct Standing {
    claim: Box<Claimed>,
    /// When `harness policy` was last read (the clock the policy is re-read on while no
    /// subscription stands).
    policy_read: Instant,
    /// When the subscription was last asked for.
    subscribed: Instant,
}

impl Standing {
    fn new(claim: Box<Claimed>) -> Standing {
        let now = Instant::now();
        Standing {
            claim,
            policy_read: now,
            subscribed: now,
        }
    }
}

/// What one tick found on a standing claim's connections.
enum Pulse {
    /// Nothing moved.
    Still,
    /// The policy may have moved: read it again.
    Reread,
    /// aterm closed the claim's connection.
    Closed,
}

impl Link {
    fn claimed(&self) -> Option<&Claimed> {
        match self {
            Link::Held(s) => Some(&s.claim),
            Link::Off | Link::Released(_) | Link::Lost { .. } => None,
        }
    }

    fn take(&mut self) -> Link {
        std::mem::replace(self, Link::Off)
    }

    /// Mark the claim lost after `why`, once; the claim is made again on the next tick.
    fn lose(&mut self, why: &str, journal: &Journal) {
        let locator = match self {
            Link::Held(s) | Link::Released(s) => s.claim.locator.clone(),
            Link::Off | Link::Lost { .. } => return,
        };
        journal.line(
            "LOST",
            &format!(
                "the claim's connection to aterm failed ({why}); it is made again now, then every {} s",
                RECLAIM_EVERY.as_secs()
            ),
        );
        *self = Link::Lost {
            locator,
            tried: None,
            why: None,
        };
    }

    /// The tick's watch over aterm: a lost claim is made again; a standing one notices a
    /// closed connection (aterm restarted, or a seamless update dropped it) now, not at the next
    /// act, and follows the policy on its `harness-policy` event (or on a clock while aterm
    /// gives no subscription).
    fn tick(&mut self, running: &Build, journal: &Journal) {
        let pulse = match self {
            Link::Off => return,
            Link::Lost { .. } => {
                self.retry(running, journal);
                return;
            }
            Link::Held(s) | Link::Released(s) => Self::pulse(s, journal),
        };
        match pulse {
            Pulse::Still => {}
            Pulse::Closed => self.lose("aterm closed the claim's connection", journal),
            Pulse::Reread => self.follow_policy(running, journal),
        }
    }

    fn pulse(s: &mut Standing, journal: &Journal) -> Pulse {
        // The claim through one plain borrow of the box, so every read and write below is a
        // place the verified lane follows (a write through the box's own deref is not).
        let claim: &mut Claimed = &mut s.claim;
        if claim.conn.closed() {
            return Pulse::Closed;
        }
        match claim.events.as_mut().map(aterm::Events::drain) {
            Some(Ok(d)) if d.policy_moved => Pulse::Reread,
            Some(Ok(_)) => Pulse::Still,
            Some(Err(e)) => {
                claim.events = None;
                s.subscribed = Instant::now();
                journal.line(
                    "NOTE",
                    &format!(
                        "aterm's event stream ended ({e}); the policy is re-read every {} s until it is back",
                        RECLAIM_EVERY.as_secs()
                    ),
                );
                Pulse::Reread
            }
            None => {
                let mut pulse = if s.policy_read.elapsed() >= RECLAIM_EVERY {
                    Pulse::Reread
                } else {
                    Pulse::Still
                };
                if s.subscribed.elapsed() >= RECLAIM_EVERY {
                    s.subscribed = Instant::now();
                    if let Ok(sock) = claim.locator.sock() {
                        claim.events =
                            aterm::Events::open(&sock, &claim.locator.sid, aterm::TIMEOUT).ok();
                        if claim.events.is_some() {
                            // A move between the old stream's end and the new one is not missed.
                            pulse = Pulse::Reread;
                        }
                    }
                }
                pulse
            }
        }
    }

    /// Read `harness policy` again and follow it: a held claim is given back when the policy
    /// is off for the session, and a given-back one made again when it is back on.
    fn follow_policy(&mut self, running: &Build, journal: &Journal) {
        let read = match self {
            Link::Held(s) | Link::Released(s) => {
                s.policy_read = Instant::now();
                s.claim.refresh_policy().cloned()
            }
            Link::Off | Link::Lost { .. } => return,
        };
        let policy = match read {
            Ok(p) => p,
            Err(why) if why.contains("answered") => return,
            Err(why) => {
                self.lose(&why, journal);
                return;
            }
        };
        let on = policy.effective && policy.serves(aterm::REQUIRED_CONTRACT);
        match (self.take(), on) {
            (Link::Held(s), false) => self.release(s, &policy, journal),
            (Link::Released(mut s), true) => {
                match s.claim.claim_again(
                    std::process::id(),
                    crate::VERSION,
                    running.version.as_deref(),
                ) {
                    Ok(()) => {
                        journal.line(
                            "CLAIMED",
                            &format!(
                                "{} holds the session again under contract 3, scope={}: aterm's harness policy is back on",
                                s.claim.holder,
                                aterm::SCOPE
                            ),
                        );
                        *self = Link::Held(s);
                    }
                    Err(why) => {
                        journal.line("UNCLAIMED", &format!("the claim was not made again: {why}"));
                        *self = Link::Released(s);
                    }
                }
            }
            (other, _) => *self = other,
        }
    }

    /// Give a held claim back because `policy` is off for the session.
    fn release(&mut self, mut s: Standing, policy: &aterm::Policy, journal: &Journal) {
        let why = if policy.serves(aterm::REQUIRED_CONTRACT) {
            format!(
                "aterm's harness policy is off for this session (why={})",
                policy.why
            )
        } else {
            format!(
                "aterm no longer serves harness contract {}",
                aterm::REQUIRED_CONTRACT
            )
        };
        match s.claim.release() {
            Ok(()) => {
                journal.line(
                    "RELEASED",
                    &format!(
                        "{} gave its claim back: {why}; nothing is relaunched or moved on aterm's word until it is back on",
                        s.claim.holder
                    ),
                );
                *self = Link::Released(s);
            }
            Err(e) => {
                *self = Link::Held(s);
                self.lose(&format!("the claim could not be given back: {e}"), journal);
            }
        }
    }

    /// Make a lost claim again: at once after the loss, then every [`RECLAIM_EVERY`].
    fn retry(&mut self, running: &Build, journal: &Journal) {
        let Link::Lost {
            locator,
            tried,
            why: last,
        } = self
        else {
            return;
        };
        if tried.is_some_and(|t| t.elapsed() < RECLAIM_EVERY) {
            return;
        }
        *tried = Some(Instant::now());
        match aterm::claim(
            locator,
            std::process::id(),
            crate::VERSION,
            running.version.as_deref(),
            aterm::TIMEOUT,
        ) {
            Ok(c) => {
                journal.line(
                    "CLAIMED",
                    &format!(
                        "{} holds the session again under contract 3, scope={}",
                        c.holder,
                        aterm::SCOPE
                    ),
                );
                *self = Link::Held(Standing::new(Box::new(c)));
            }
            // Each new reason once: a retry every 5 s must not fill the journal.
            Err(why) if last.as_deref() != Some(why.as_str()) => {
                journal.line("UNCLAIMED", &format!("the claim was not made again: {why}"));
                *last = Some(why);
            }
            Err(_) => {}
        }
    }

    /// One fresh read of aterm for the safe point.
    fn read(&mut self, journal: &Journal) -> Witness {
        let Link::Held(s) = self else {
            return Witness::Aterm(Err("this harness holds no claim now".to_string()));
        };
        s.policy_read = Instant::now();
        let c = &mut s.claim;
        let policy = c.refresh_policy().cloned();
        let read = policy.and_then(|policy| {
            c.screen().map(|screen| {
                Box::new(AtermRead {
                    screen,
                    policy,
                    holder: c.holder.clone(),
                })
            })
        });
        if let Err(why) = &read
            && !why.contains("answered")
        {
            self.lose(why, journal);
        }
        Witness::Aterm(read)
    }

    /// Whether a crash may be relaunched now, on ONE FRESH read of aterm (never the policy a
    /// claim was made under): the claim is this harness's and proven, the policy is on for the
    /// session and `[harness] relaunch = true`. A policy found off gives the claim back; a claim
    /// aterm no longer shows is made again.
    ///
    /// # Errors
    /// Why the crash is not relaunched.
    fn may_relaunch(&mut self, running: &Build, journal: &Journal) -> Result<(), String> {
        match self {
            Link::Off => {
                return Err("this harness holds no claim with relaunch scope".to_string());
            }
            Link::Lost { .. } => {
                return Err(
                    "this harness holds no claim now (its connection to aterm failed)".to_string(),
                );
            }
            Link::Released(_) => {
                return Err(
                    "the claim was given back: aterm's harness policy is off for this session"
                        .to_string(),
                );
            }
            Link::Held(_) => {}
        }
        let boxed = match self.read(journal) {
            Witness::Aterm(Ok(read)) => read,
            Witness::Aterm(Err(why)) => return Err(format!("no fresh read of aterm: {why}")),
            Witness::Request => return Err("no fresh read of aterm".to_string()),
        };
        // One plain borrow of the box, so every read below is a place the verified lane follows.
        let read: &AtermRead = &boxed;
        if !read.screen.held_by(&read.holder) {
            let why = format!(
                "aterm does not show this harness's proven claim (supervisor={} supervisor_proven={})",
                read.screen.supervisor.as_deref().unwrap_or("-"),
                u8::from(read.screen.proven)
            );
            self.lose(&why, journal);
            self.retry(running, journal);
            return Err(why);
        }
        if !read.policy.effective || !read.policy.serves(aterm::REQUIRED_CONTRACT) {
            if let Link::Held(s) = self.take() {
                self.release(s, &read.policy, journal);
            }
            return Err(format!(
                "aterm's harness policy is off for this session (why={})",
                read.policy.why
            ));
        }
        match read.policy.relaunch {
            Some(true) => Ok(()),
            Some(false) => Err("[harness] relaunch is off".to_string()),
            None => Err("the policy reports no relaunch switch".to_string()),
        }
    }

    /// Whether a person may have typed in the last `since` (input after the harness's own
    /// SIGTERM): aterm's `human_ms` under a held claim, and the terminal's input clock always.
    /// Anything unread counts as typed.
    fn typed_within(&mut self, since: Duration, journal: &Journal) -> bool {
        let tty_says = procs::tty_quiet(io::stdin().as_fd(), SystemTime::now())
            .is_none_or(|q| q.input < since);
        if tty_says {
            return true;
        }
        if matches!(self, Link::Off) {
            return false;
        }
        match self.read(journal) {
            Witness::Aterm(Ok(boxed)) => {
                let read: &AtermRead = &boxed;
                match read.screen.human {
                    aterm::Human::Never => false,
                    aterm::Human::Ago(ago) => ago < since,
                    aterm::Human::Unknown => true,
                }
            }
            _ => true,
        }
    }

    /// Re-announce the claim after a move onto `running` (its new `vendor=`).
    fn announce(&mut self, running: &Build, journal: &Journal) {
        let Link::Held(s) = self else {
            return;
        };
        if let Err(why) = s.claim.reclaim(
            std::process::id(),
            crate::VERSION,
            running.version.as_deref(),
        ) {
            journal.line(
                "NOTE",
                &format!(
                    "the claim was not re-announced for grok {}: {why}",
                    running.version_label()
                ),
            );
            self.lose(&why, journal);
        }
    }
}

/// The signal a crash ended the child with, or `None` when it did not crash: a fault or an
/// abort (`SEGV`, `BUS`, `ILL`, `ABRT`, `FPE`, `TRAP`, `SYS`). An exit by code is the child's
/// own end, and `KILL`, `TERM`, `HUP`, `INT` and `QUIT` come from a person, the terminal or a
/// remedy, so none of them is relaunched.
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
    Ended(libc::c_int),
}

fn reap(pid: libc::pid_t) -> Reaped {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: `waitpid` on our own child, writing its status into the live local.
        let r = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG | libc::WUNTRACED) };
        if r == 0 {
            return Reaped::Running;
        }
        if r == pid {
            if libc::WIFSTOPPED(status) {
                return Reaped::Stopped;
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

/// Run the session to its end and answer the wait status to end like.
#[must_use]
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
        claim,
    } = s;
    let modes = Modes::take();
    let mut link = claim.map_or(Link::Off, |c| Link::Held(Standing::new(c)));
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
    let standing = match link.claimed() {
        Some(c) => format!(
            "{} holds the session under aterm contract 3, scope={} (aterm {}; policy followed {})",
            c.holder,
            aterm::SCOPE,
            c.conn.version,
            if c.events.is_some() {
                "on its harness-policy event"
            } else {
                "on a clock: aterm gave no event stream"
            }
        ),
        None => "not under aterm (SIGUSR1 asks for a move)".to_string(),
    };
    journal.line(
        "START",
        &format!(
            "{} {} runs grok {} (build {}) as pid {child}; {}; {standing}",
            crate::NAME,
            crate::VERSION,
            build.version_label(),
            build.id,
            plan_words(&plan)
        ),
    );
    let carried: Option<Carried> = match &plan {
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
    // The harness's own SIGTERM for a move, when it was sent, and the move's target.
    let mut moving: Option<(Instant, Build)> = None;
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
    let mut crashes = 0_u32;
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
        for sig in signals.wait(timing.poll) {
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
                    journal.line("CONT", "continued; grok continued with it");
                }
                Reaped::Ended(status) => {
                    // A crashed child's raw mode never leaks into the next child or the shell.
                    modes.restore();
                    let ran = started.elapsed();
                    let ours = moving
                        .take()
                        .filter(|(at, _)| !ending && at.elapsed() <= timing.exit_grace);
                    let late = unanswered_term.take();
                    let just_moved = moved
                        .take()
                        .filter(|(at, _)| !ending && at.elapsed() < timing.min_run);
                    // 1. The move's own exit, or (2) a late one after its unanswered SIGTERM
                    //    with nobody typing since: finish the move, on the move's target.
                    let mut late_not_ours: Option<String> = None;
                    let finish = match (ours, &late) {
                        (Some((_, target)), _) => Some((target, "")),
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
                            } else if link.typed_within(after, journal) {
                                late_not_ours = Some(
                                    "a person may have typed since, so it reads as their quit"
                                        .to_string(),
                                );
                                None
                            } else {
                                Some((target.clone(), " after a late exit"))
                            }
                        }
                        _ => None,
                    };
                    if let (Some((target, how)), Some(c)) = (finish, carried.as_ref()) {
                        if target.id != running.id
                            && !store::build(layout, target.id)
                                .is_ok_and(|b| !crate::is_self(&b.bin))
                        {
                            failed.insert(target.id);
                        }
                        let onto = relaunch_target(layout, &running, &target, &failed);
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
                                running = onto;
                                started = Instant::now();
                                waiting = None;
                                link.announce(&running, journal);
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
                                running = back;
                                started = Instant::now();
                                waiting = None;
                                link.announce(&running, journal);
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
                    // 4. A crash, relaunched under the claim's `relaunch` on a fresh read.
                    if let Some(c) = carried.as_ref()
                        && let Some(sig) = crash_signal(status)
                        && !ending
                    {
                        let why_not = if ran < timing.min_run {
                            Some(format!(
                                "it ran {} ms, under {} ms: a refused start is never looped on",
                                ran.as_millis(),
                                timing.min_run.as_millis()
                            ))
                        } else if crashes >= MAX_CRASH_RELAUNCHES {
                            Some(format!(
                                "{MAX_CRASH_RELAUNCHES} crashes were relaunched already"
                            ))
                        } else {
                            link.may_relaunch(&running, journal).err()
                        };
                        match why_not {
                            Some(why) => journal.line(
                                "EXIT",
                                &format!("grok crashed (signal {sig}); not relaunched: {why}"),
                            ),
                            None => {
                                let pause = timing.crash_pause.saturating_mul(1 << crashes);
                                crashes = crashes.saturating_add(1);
                                journal.line(
                                    "CRASH",
                                    &format!(
                                        "grok {} crashed (signal {sig}) after {} ms; relaunch {crashes} of {MAX_CRASH_RELAUNCHES} on session {} in {} ms",
                                        running.version_label(),
                                        ran.as_millis(),
                                        c.session,
                                        pause.as_millis()
                                    ),
                                );
                                if pause_unless_ended(signals, pause, timing.poll) {
                                    journal.line(
                                        "EXIT",
                                        "the session ended during the crash pause; not relaunched",
                                    );
                                    return status;
                                }
                                let onto = relaunch_target(layout, &running, &running, &failed);
                                let attempt =
                                    match relaunch(layout, &onto, c, &ignored, depth, journal) {
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
                                                "after a crash: grok {} as pid {pid}: {}",
                                                onto.version_label(),
                                                c.relaunch_argv().join(" ")
                                            ),
                                        );
                                        let moved_build = onto.id != running.id;
                                        if moved_build {
                                            moved = Some((Instant::now(), running.clone()));
                                            moves = moves.saturating_add(1);
                                        }
                                        child = pid;
                                        running = onto;
                                        started = Instant::now();
                                        waiting = None;
                                        if moved_build {
                                            link.announce(&running, journal);
                                        }
                                        continue;
                                    }
                                    Err(Refusal::SignedOut) => {
                                        let why = signed_out_words(
                                            &format!("grok crashed (signal {sig})"),
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
        if moving
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() > timing.exit_grace)
        {
            unanswered_term = moving.take();
            journal.line(
                "HOLD",
                &format!(
                    "grok did not exit within {} ms of the harness's SIGTERM; it runs on, on {} (never killed harder)",
                    timing.exit_grace.as_millis(),
                    running.version_label()
                ),
            );
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
                    if link.claimed().is_some() {
                        "aterm's read, or SIGUSR1"
                    } else {
                        "SIGUSR1"
                    }
                ),
            );
        }
        link.tick(&running, journal);
        // Under a claim, aterm's read is the witness on every tick a newer build waits; a
        // request is the override.
        let witness = if request {
            Some(Witness::Request)
        } else if newer.is_some()
            && moving.is_none()
            && !unanswered_term
                .as_ref()
                .is_some_and(|(_, t)| Some(t.id) == newer.as_ref().map(|b| b.id))
            && !ending
            && uncarried.is_none()
            && link.claimed().is_some()
        {
            Some(link.read(journal))
        } else {
            None
        };
        if let Some(witness) = witness {
            let asked = witness == Witness::Request;
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
                        moving = Some((Instant::now(), target));
                        unanswered_term = None;
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

/// Wait `pause`, watching for a HUP or TERM; `true` when one came (the session is ending).
fn pause_unless_ended(signals: &Signals, pause: Duration, poll: Duration) -> bool {
    let until = Instant::now() + pause;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        if signals
            .wait(left.min(poll))
            .iter()
            .any(|s| *s == libc::SIGHUP || *s == libc::SIGTERM)
        {
            return true;
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
