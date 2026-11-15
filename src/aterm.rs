// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! grok-alab's side of its link to aterm. The link itself is the shared library `alab-wire`
//! (`vendor/alab-harness/crates/alab-wire`, the pinned alab-harness v0.1.0 copy): the wire, the
//! claim at contract 4 or 3 with its guards (the roster under contract 3, the scope read back),
//! the [`Link`] state machine (held, lost and claimed again, given back while the policy is off,
//! the policy followed on its event), aterm's safe-point and relaunch judgements, and the crash
//! ladder. This module holds only what is this harness's own:
//!
//! - who it is to aterm ([`config`]: `grok-alab` running `grok`, claiming [`SCOPES`]);
//! - whether a launch is inside aterm ([`hosted`]) and leaves the harness out ([`left_out`]);
//! - the clock its link runs on ([`now_ms`], which stops while the machine sleeps);
//! - what it claims for a child ([`child`], [`process_of`]);
//! - what it needs of a launch's grant ([`takes_both`], [`granted`]);
//! - when a move a person asked for by hand may go ahead on aterm's facts ([`unheld`]);
//! - how the link's notes reach the journal ([`journal`]).
//!
//! This harness never types into the session: `relaunch` and `upgrade` type nothing, so it
//! never sends `send`, `key`, `paste` or `turn`.

use std::ffi::OsString;

use alab_wire::{Child, ChildProcess, Config, Facts, Link, SafePoint, Scope, Warrant};

use crate::env::Env;
use crate::journal::Journal;

/// This harness's id: its row in aterm's roster and registry, and the holder `<id>@<pid>`.
pub const HOLDER_ID: &str = "grok-alab";

/// The word of the vendor it runs.
pub const VENDOR_WORD: &str = "grok";

/// The duties this harness performs and claims from aterm: bringing a crashed grok back on its
/// session (`relaunch`, which also covers a restart aterm asks for), and moving it onto a newer
/// grok build (`upgrade`). The typing scopes stay unclaimed: this harness types nothing.
pub const SCOPES: [Scope; 2] = [Scope::Relaunch, Scope::Upgrade];

/// The variable that leaves the harness out of a session (`--no-alab-harness` on the twin).
pub const NO_HARNESS_ENV: &str = "ATERM_NO_HARNESS";

/// Who this harness is to aterm.
///
/// # Panics
/// Never: the id and vendor word are fixed lowercase words, [`SCOPES`] is not empty, and the
/// build refuses a [`crate::VERSION`] that is not `MAJOR.MINOR.0[+g<sha>]`
/// ([`crate::is_pack_version`]), which is a claim's `version=`. A unit test makes this config.
#[must_use]
pub fn config() -> Config {
    Config::new(HOLDER_ID, VENDOR_WORD, &SCOPES, crate::VERSION)
        .expect("grok-alab's id, vendor word, scopes and version are a config aterm reads")
}

/// `env` as the lookup `alab-wire` reads a session from.
pub fn lookup(env: &Env) -> impl Fn(&str) -> Option<OsString> + '_ {
    |name| env.get(name)
}

/// Whether this process runs inside aterm, by the marks aterm gives the programs it starts
/// (`alab-wire`'s defaults): `TERM_PROGRAM=aterm`, `ATERM_CHILD=1`, or any `ATERM_SESSION_ID`.
#[must_use]
pub fn hosted(env: &Env) -> bool {
    config().env().hosted_by(lookup(env))
}

/// Whether the person left the harness out (`ATERM_NO_HARNESS=1`, the one value the twin's
/// hand-off sets).
#[must_use]
pub fn left_out(env: &Env) -> bool {
    env.get(NO_HARNESS_ENV).is_some_and(|v| v == "1")
}

/// The link's clock: milliseconds since boot on a system clock that stops while the machine
/// sleeps, the clock `std::time::Instant` reads (`CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC`
/// on Linux). The crash ladder's healthy run and the 45 s away budget run on it, so a closed lid
/// neither forgives a crash streak nor spends the time aterm has to come back.
#[must_use]
pub fn now_ms() -> u64 {
    uptime_ns() / 1_000_000
}

/// The clock [`now_ms`] reads.
#[cfg(target_vendor = "apple")]
const UPTIME_CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;

/// The clock [`now_ms`] reads.
#[cfg(not(target_vendor = "apple"))]
const UPTIME_CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

/// Nanoseconds on [`UPTIME_CLOCK`]. macOS's `CLOCK_MONOTONIC` keeps counting while the machine
/// sleeps, so macOS reads `CLOCK_UPTIME_RAW`; Linux's `CLOCK_MONOTONIC` stops in suspend.
fn uptime_ns() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a writable timespec that outlives the call, which writes only it. The
    // clock is one every Rust on the platform reads for `Instant`, so the call does not fail;
    // were it to, `now` stays zero, which reads as no time passed.
    let _ = unsafe { libc::clock_gettime(UPTIME_CLOCK, &raw mut now) };
    u64::try_from(now.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or(0))
}

/// What the claim says about the child: its vendor build and, once it runs, its pid and kernel
/// start time (`child=` under contract 4). A start time that cannot be read is left out rather
/// than guessed. grok-alab runs grok on the machine's one sign-in, so no `login=`.
#[must_use]
pub fn child(vendor: Option<&str>, process: Option<ChildProcess>) -> Child<'_> {
    Child {
        vendor,
        login: None,
        process,
    }
}

/// The kernel identity of a child just started, for its `child=`.
#[must_use]
pub fn process_of(pid: libc::pid_t) -> Option<ChildProcess> {
    u32::try_from(pid)
        .ok()
        .and_then(|pid| ChildProcess::of(pid).ok())
}

/// Whether the standing claim holds both of [`SCOPES`]: what a launch needs before grok runs
/// under it (a launch granted less passes through, Decision 44).
#[must_use]
pub fn takes_both(link: &Link) -> bool {
    SCOPES.iter().all(|scope| link.takes(*scope))
}

/// The standing claim's grant as journal words (`relaunch` or `-`).
#[must_use]
pub fn granted(link: &Link) -> String {
    link.grant()
        .map_or_else(|| "-".to_string(), alab_wire::Grant::words)
}

/// Whether a move a person asked for by hand (`SIGUSR1`) may go ahead on aterm's fresh facts
/// ([`Link::read`]), by `alab-wire`'s rule for a move the harness judges itself
/// ([`alab_wire::claim::judge_own_move`] with [`Warrant::Person`]): the claim is still
/// `holder`'s, proven and takes `upgrade`; aterm reads no halt and no other driver's hand. The
/// person's word stands over the policy's `upgrade` and `upgrade_ask` switches; the terminal's
/// input quiet and the local gates are the harness's own ([`crate::safepoint`]).
///
/// # Errors
/// The first fact that holds the move (`aterm <field>=<value>`).
pub fn unheld(facts: &Facts, holder: &str) -> Result<(), String> {
    match alab_wire::claim::judge_own_move(&facts.policy, &facts.status, holder, Warrant::Person) {
        SafePoint::Safe => Ok(()),
        SafePoint::Hold(hold) => Err(format!("aterm {}={}", hold.field, hold.value)),
    }
}

/// Write the link's notes to the journal: `aterm claimed holder=… contract=4 …` is
/// `CLAIMED holder=… contract=4 …`, and likewise `UNCLAIMED`, `LOST`, `RELEASED`,
/// `REANNOUNCED`, `NARROWED`, `WIDENED` and `REGRANTED`. The last note's words, when there was
/// one.
pub fn journal(link: &mut Link, journal: &Journal) -> Option<String> {
    let mut last = None;
    for line in link.take_notes() {
        let rest = line.strip_prefix("aterm ").unwrap_or(&line);
        let (verb, detail) = rest.split_once(' ').unwrap_or((rest, ""));
        journal.line(&verb.to_ascii_uppercase(), detail);
        last = Some(detail.to_string());
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    #[test]
    fn this_harness_is_grok_alab_claiming_relaunch_and_upgrade() {
        let config = config();
        assert_eq!(config.id(), "grok-alab");
        assert_eq!(config.vendor(), "grok");
        assert_eq!(config.scopes(), [Scope::Relaunch, Scope::Upgrade]);
        assert_eq!(config.version(), crate::VERSION);
        assert_eq!(config.holder(77), "grok-alab@77");
    }

    #[test]
    fn aterm_is_named_by_its_marks_and_only_a_one_leaves_the_harness_out() {
        for inside in [
            &[("TERM_PROGRAM", "aterm")][..],
            &[("ATERM_CHILD", "1")],
            &[("ATERM_SESSION_ID", "")],
        ] {
            assert!(
                hosted(&Env::from_pairs(inside.iter().copied())),
                "{inside:?}"
            );
        }
        for outside in [
            &[][..],
            &[("TERM_PROGRAM", "Apple_Terminal")],
            &[("ATERM_CHILD", "0")],
        ] {
            assert!(
                !hosted(&Env::from_pairs(outside.iter().copied())),
                "{outside:?}"
            );
        }
        assert!(left_out(&env(&[(NO_HARNESS_ENV, "1")])));
        for value in ["", "0", "true", " 1"] {
            assert!(
                !left_out(&Env::from_pairs([(NO_HARNESS_ENV, value)])),
                "{value:?}"
            );
        }
        assert_eq!(NO_HARNESS_ENV, "ATERM_NO_HARNESS");
    }

    #[test]
    fn outside_aterm_the_link_is_absent_and_holds_nothing() {
        let e = env(&[]);
        let link = Link::launch_with(
            config(),
            std::process::id(),
            child(Some("1.0.70"), None),
            now_ms(),
            lookup(&e),
        );
        assert!(link.is_absent() && !link.claims() && !link.judges());
        assert!(!takes_both(&link));
        assert_eq!(granted(&link), "-");
    }

    #[test]
    fn the_clock_is_monotonic() {
        let first = now_ms();
        let second = now_ms();
        assert!(second >= first);
        assert!(first > 0, "milliseconds since boot");
    }

    /// The link's clock leaves out the time the Mac slept, as `Instant` does: it reads macOS's
    /// `CLOCK_UPTIME_RAW`, not `CLOCK_MONOTONIC`, which counts sleep. On a Mac that has slept
    /// since boot the two differ by that sleep, so a `CLOCK_MONOTONIC` reading falls outside the
    /// window this reads around it.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_clock_stops_while_the_mac_sleeps() {
        let uptime_ms = || {
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `now` is a writable timespec that outlives the call, which writes only it.
            let rc = unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &raw mut now) };
            assert_eq!(rc, 0);
            u64::try_from(now.tv_sec).unwrap() * 1_000
                + u64::try_from(now.tv_nsec).unwrap() / 1_000_000
        };
        let before = uptime_ms();
        let now = now_ms();
        let after = uptime_ms();
        assert!(
            (before..=after).contains(&now),
            "{now} is not between {before} and {after}"
        );
    }

    #[test]
    fn a_child_names_its_build_and_process_and_no_login() {
        let process = process_of(libc::pid_t::try_from(std::process::id()).unwrap())
            .expect("this process's own start time");
        let named = child(Some("1.0.70"), Some(process));
        assert_eq!(named.vendor, Some("1.0.70"));
        assert_eq!(named.login, None);
        assert_eq!(named.process, Some(process));
        assert_eq!(process_of(-1), None);
    }

    fn facts(policy: &str, scope: &str, status: &str) -> Facts {
        Facts {
            policy: alab_wire::Policy::parse(&format!(
                "OK gen=1 contract=2,3,4 relaunch=true human_grace_s=120 {policy}"
            ))
            .unwrap(),
            status: alab_wire::Status::parse(&format!(
                "OK schema=1 supervisor=grok-alab%4077 supervisor_proven=1 \
                 supervisor_scope={scope} {status}"
            ))
            .unwrap(),
        }
    }

    /// A person's `SIGUSR1` under a claim: aterm's switches do not hold it (the person is who
    /// `upgrade_ask` waits for), a halt, another driver's hand and a claim without `upgrade` do.
    #[test]
    fn a_move_a_person_asked_for_needs_the_upgrade_duty_no_halt_and_no_driver() {
        let holder = "grok-alab@77";
        let on = "effective=on why=enabled upgrade=true upgrade_ask=false";
        let both = "relaunch,upgrade";
        let free = "hold=0 hand=-";
        assert_eq!(unheld(&facts(on, both, free), holder), Ok(()));
        for asks in [
            "effective=on why=enabled upgrade=true upgrade_ask=true",
            "effective=on why=enabled upgrade=false upgrade_ask=false",
        ] {
            assert_eq!(unheld(&facts(asks, both, free), holder), Ok(()), "{asks}");
        }
        for (policy, scope, status, why) in [
            (
                "effective=off why=hold upgrade=true upgrade_ask=false",
                both,
                free,
                "aterm hold=1",
            ),
            (on, both, "hold=1 hand=-", "aterm hold=1"),
            (
                on,
                both,
                "hold=0 hand=lease:driver%404",
                "aterm hand=lease:driver%25404",
            ),
            (on, "relaunch", free, "aterm supervisor_scope=relaunch"),
        ] {
            assert_eq!(
                unheld(&facts(policy, scope, status), holder),
                Err(why.to_string()),
                "{policy} / {scope} / {status}"
            );
        }
        assert_eq!(
            unheld(&facts(on, both, free), "grok-alab@78"),
            Err("aterm supervisor=grok-alab@77".to_string())
        );
    }

    /// Every note the link makes reaches the journal as its own event: the launch's claim is
    /// `CLAIMED holder=… contract=4 …`, and the last note's words are returned.
    #[test]
    fn the_links_notes_become_journal_events() {
        let scratch = crate::scratch::Scratch::new("aterm-journal");
        let journal = Journal::open(&Env::from_pairs([(
            crate::journal::STATE_ENV,
            scratch.display().to_string(),
        )]));
        let config = config();
        let fake = alab_wire::fake::FakeAterm::for_harness("grok-journal", &config);
        let mut link = Link::launch_with(
            config,
            77,
            child(Some("1.0.70"), None),
            now_ms(),
            fake.lookup(&alab_wire::EnvNames::default()),
        );
        assert!(link.judges(), "{:?}", link.take_notes());
        let last = super::journal(&mut link, &journal).expect("the claim's note");
        assert!(last.starts_with("holder=grok-alab@77 contract=4 scope=relaunch,upgrade"));
        assert!(takes_both(&link));
        assert_eq!(granted(&link), "relaunch,upgrade");
        let text = std::fs::read_to_string(journal.path().unwrap()).unwrap();
        assert!(
            text.contains(" CLAIMED holder=grok-alab@77 contract=4 scope=relaunch,upgrade"),
            "{text}"
        );
        assert_eq!(super::journal(&mut link, &journal), None, "each note once");
        link.release(now_ms());
    }
}
