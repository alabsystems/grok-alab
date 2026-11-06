// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SAFE POINT: when the harness may end its child to move it onto a newer build.
//!
//! The harness cannot see the screen (it inherits the terminal and proxies nothing), so it
//! cannot tell by itself that a turn has ended, the composer is empty and nobody is typing.
//! A move therefore needs a WITNESS for what only the screen shows ([`Witness`]):
//!
//! - **aterm's read** ([`Witness::Aterm`]), under a proven contract 3 claim
//!   ([`crate::aterm`]): one fresh `status` and `harness policy` read, checked here against
//!   every generic condition of aterm's harness design (`docs/DESIGN-harness-repos-2026-10-24.md`
//!   §4.2), each holding the move when it fails or is not reported ([`screen_holds`]). No
//!   signal is needed: a newer build moves at the first read that passes.
//! - **a request** ([`Witness::Request`]): `SIGUSR1` to the harness, which says "the session is
//!   at a safe point now". Whoever sends it vouches for the screen; the terminal's input quiet
//!   is checked in place of aterm's read. Outside aterm it is the only witness; under a claim it
//!   stays as an override.
//!
//! And every local gate below, each a necessary condition the harness checks itself, each
//! holding the move (never forcing it) when it fails or cannot be read:
//! - the session is not ending (no HUP or TERM was passed on) and the child is not stopped;
//! - the launch is one the harness can carry ([`crate::lanes::Plan::Session`]);
//! - a newer build than the running one is installed and ready;
//! - that build would start signed in ([`crate::credentials`]): the relaunch is the harness's
//!   own act, and a signed-out grok opens xAI's sign-in in the desk's browser;
//! - the child runs nothing under it (no tool command, subagent or helper process);
//! - its terminal wrote no output for [`DEFAULT_QUIET`] (aterm's own idle settle,
//!   `QUIET_S = 20`, `crates/aterm-agent/src/harness/upgrade.rs`); and, for a request, read no
//!   input for [`DEFAULT_HUMAN_GRACE`] (aterm's `[harness] human_grace_s` default of 120,
//!   `crates/aterm-agent/src/supervise/config.rs`).
//!
//! A request that meets a failed gate is refused and dropped (the journal says which gate);
//! the sender asks again. aterm's read is taken again on the next tick. Fail closed: a busy
//! child is never ended.

use std::time::Duration;

use crate::aterm::{Human, Policy, Screen};
use crate::procs::TtyQuiet;

/// How long the terminal must have written nothing (aterm `upgrade::QUIET_S`).
pub const DEFAULT_QUIET: Duration = Duration::from_secs(20);

/// How long the terminal must have read nothing typed (aterm `human_grace_s` default).
pub const DEFAULT_HUMAN_GRACE: Duration = Duration::from_secs(120);

/// What the harness knows when a safe-point request arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// A HUP or TERM was passed on: the session is ending.
    pub ending: bool,
    /// The child is stopped (job control).
    pub stopped: bool,
    /// The harness already sent its own SIGTERM and waits for the exit.
    pub moving: bool,
    /// Why the launch cannot be carried, or `None` when it can.
    pub uncarried: Option<String>,
    /// The newer, ready build's version, or `None` when no newer build is installed.
    pub newer: Option<String>,
    /// Whether the newer build, relaunched now, would start signed in
    /// ([`crate::credentials::signed_in`]).
    pub signed_in: bool,
    /// How many processes run under the child, or `None` when that cannot be read.
    pub descendants: Option<usize>,
    /// The terminal's quiet times, or `None` when there is no terminal to read.
    pub tty: Option<TtyQuiet>,
    /// The output quiet required.
    pub quiet: Duration,
    /// The input quiet a request requires.
    pub human_grace: Duration,
    /// What vouches for the screen.
    pub witness: Witness,
}

/// What vouches for what only the screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Witness {
    /// A `SIGUSR1`: its sender vouches; the terminal's input quiet is checked.
    Request,
    /// aterm's read under a proven claim, or why there is none this time.
    Aterm(Result<Box<AtermRead>, String>),
}

/// One fresh read of aterm: the session's `status` and its `harness policy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AtermRead {
    /// `status`.
    pub screen: Screen,
    /// `harness policy`.
    pub policy: Policy,
    /// This harness's holder word (`grok-alab@<pid>`).
    pub holder: String,
}

/// Why aterm's read holds the move, or `None` when every generic condition of the safe point
/// holds (aterm's harness design §4.2). A field aterm does not report holds the move.
///
/// `agent=` must be `idle`: aterm's `prompt` is a permission box (`program_state=blocked`), not
/// a turn that ended, so it never passes, whatever the design's list says.
#[must_use]
pub fn screen_holds(r: &AtermRead) -> Option<String> {
    let s = &r.screen;
    if !s.held_by(&r.holder) {
        return Some(format!(
            "aterm does not show this harness's proven claim (supervisor={} supervisor_proven={})",
            s.supervisor.as_deref().unwrap_or("-"),
            u8::from(s.proven)
        ));
    }
    if !r.policy.effective {
        return Some(format!(
            "aterm's harness policy is off here (why={})",
            r.policy.why
        ));
    }
    match r.policy.upgrade {
        Some(true) => {}
        Some(false) => return Some("[harness] upgrade = false".to_string()),
        None => return Some("the policy reports no upgrade switch".to_string()),
    }
    match r.policy.upgrade_ask {
        Some(false) => {}
        Some(true) => {
            return Some(
                "[harness] upgrade_ask = true: a person says when (SIGUSR1 to the harness)"
                    .to_string(),
            );
        }
        None => return Some("the policy reports no upgrade_ask switch".to_string()),
    }
    match s.agent.as_deref() {
        Some("idle") => {}
        other => {
            return Some(format!(
                "aterm reads agent={} (a move needs idle: the turn over, no box, question, wall or survey)",
                other.unwrap_or("absent")
            ));
        }
    }
    match s.composer.as_deref() {
        Some("empty" | "placeholder") => {}
        Some(other) => {
            return Some(format!(
                "aterm reads composer={other} (a draft is never dropped)"
            ));
        }
        None => {
            return Some(
                "aterm reports no composer= (contract 3 item 9), so a draft cannot be ruled out"
                    .to_string(),
            );
        }
    }
    let grace = r.policy.human_grace.unwrap_or(DEFAULT_HUMAN_GRACE);
    match s.human {
        Human::Never => {}
        Human::Ago(ago) if ago >= grace => {}
        Human::Ago(ago) => {
            return Some(format!(
                "a person typed {} ms ago (the grace is {} ms)",
                ago.as_millis(),
                grace.as_millis()
            ));
        }
        Human::Unknown => return Some("aterm reports no human_ms".to_string()),
    }
    let exact = |key: &str, value: Option<&str>, want: &str| -> Option<String> {
        (value != Some(want)).then(|| format!("aterm reads {key}={}", value.unwrap_or("absent")))
    };
    exact("hand", s.hand.as_deref(), "-")
        .or_else(|| exact("hold", s.hold.as_deref(), "0"))
        .or_else(|| exact("input", s.input.as_deref(), "clear"))
        .or_else(|| exact("need", s.need.as_deref(), "-"))
}

/// The verdict on a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// End the child with SIGTERM and relaunch it on the newer build.
    Move,
    /// Leave the child alone, for this reason.
    Hold(String),
}

/// The verdict for `f`: [`Decision::Move`] only when every gate passes.
#[must_use]
pub fn decide(f: &Facts) -> Decision {
    let hold = |why: String| Decision::Hold(why);
    if f.ending {
        return hold("the session is ending (a HUP or TERM was passed on)".to_string());
    }
    if f.moving {
        return hold("a move is already under way".to_string());
    }
    if f.stopped {
        return hold("grok is stopped".to_string());
    }
    let Some(newer) = &f.newer else {
        return hold("no newer grok build is installed".to_string());
    };
    if let Some(why) = &f.uncarried {
        return hold(format!("this launch cannot be carried onto {newer}: {why}"));
    }
    if !f.signed_in {
        return hold(format!(
            "grok {newer} would start signed out (no GROK_AUTH, XAI_API_KEY or login file it accepts), and a \
             signed-out grok opens xAI's sign-in in a browser"
        ));
    }
    match f.descendants {
        None => return hold("busy: what runs under grok cannot be read here".to_string()),
        Some(0) => {}
        Some(n) => {
            return hold(format!(
                "busy: {n} process(es) run under grok (a tool command, a subagent or a helper)"
            ));
        }
    }
    let Some(tty) = f.tty else {
        return hold("no terminal to read the session's quiet from".to_string());
    };
    if tty.output < f.quiet {
        return hold(format!(
            "busy: grok wrote to the terminal {} ms ago (quiet needs {} ms)",
            tty.output.as_millis(),
            f.quiet.as_millis()
        ));
    }
    match &f.witness {
        Witness::Request => {
            if tty.input < f.human_grace {
                return hold(format!(
                    "someone typed {} ms ago (the grace is {} ms)",
                    tty.input.as_millis(),
                    f.human_grace.as_millis()
                ));
            }
        }
        Witness::Aterm(Err(why)) => return hold(format!("no read of aterm: {why}")),
        Witness::Aterm(Ok(read)) => {
            if let Some(why) = screen_holds(read) {
                return hold(why);
            }
        }
    }
    Decision::Move
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_facts() -> Facts {
        Facts {
            ending: false,
            stopped: false,
            moving: false,
            uncarried: None,
            newer: Some("1.0.71".to_string()),
            signed_in: true,
            descendants: Some(0),
            tty: Some(TtyQuiet {
                input: Duration::from_secs(600),
                output: Duration::from_secs(600),
            }),
            quiet: DEFAULT_QUIET,
            human_grace: DEFAULT_HUMAN_GRACE,
            witness: Witness::Request,
        }
    }

    fn quiet_read() -> AtermRead {
        AtermRead {
            screen: Screen::parse(
                "OK schema=1 supervisor=grok-alab@9 supervisor_proven=1 agent=idle \
                 composer=empty human_ms=- hand=- hold=0 input=clear need=-",
            )
            .unwrap(),
            policy: Policy::parse(
                "OK gen=1 effective=on why=enabled contract=2,3 upgrade=true upgrade_ask=false \
                 relaunch=true human_grace_s=120",
            )
            .unwrap(),
            holder: "grok-alab@9".to_string(),
        }
    }

    fn by_aterm(read: AtermRead) -> Facts {
        let mut f = quiet_facts();
        // aterm's read replaces the terminal's input quiet: a terminal typed into a moment ago
        // (aterm's own continuation, say) does not hold it, aterm's human_ms does.
        f.tty = Some(TtyQuiet {
            input: Duration::ZERO,
            output: Duration::from_secs(600),
        });
        f.witness = Witness::Aterm(Ok(Box::new(read)));
        f
    }

    fn held(f: &Facts) -> String {
        match decide(f) {
            Decision::Hold(why) => why,
            Decision::Move => panic!("moved: {f:?}"),
        }
    }

    #[test]
    fn a_quiet_carried_session_with_a_newer_build_moves() {
        assert_eq!(decide(&quiet_facts()), Decision::Move);
    }

    #[test]
    fn every_gate_holds_the_move_on_its_own() {
        let mut f = quiet_facts();
        f.ending = true;
        assert!(held(&f).contains("ending"));

        let mut f = quiet_facts();
        f.moving = true;
        assert!(held(&f).contains("under way"));

        let mut f = quiet_facts();
        f.stopped = true;
        assert!(held(&f).contains("stopped"));

        let mut f = quiet_facts();
        f.newer = None;
        assert!(held(&f).contains("no newer"));

        let mut f = quiet_facts();
        f.uncarried = Some("--continue".to_string());
        assert!(held(&f).contains("cannot be carried"));

        let mut f = quiet_facts();
        f.signed_in = false;
        assert!(held(&f).contains("would start signed out"));

        let mut f = quiet_facts();
        f.descendants = Some(2);
        assert!(held(&f).contains("2 process"));

        let mut f = quiet_facts();
        f.descendants = None;
        assert!(held(&f).contains("cannot be read"));

        let mut f = quiet_facts();
        f.tty = None;
        assert!(held(&f).contains("no terminal"));

        let mut f = quiet_facts();
        f.tty = Some(TtyQuiet {
            input: Duration::from_secs(600),
            output: Duration::from_secs(19),
        });
        assert!(held(&f).contains("wrote to the terminal"));

        let mut f = quiet_facts();
        f.tty = Some(TtyQuiet {
            input: Duration::from_secs(119),
            output: Duration::from_secs(600),
        });
        assert!(held(&f).contains("typed"));
    }

    #[test]
    fn aterms_read_moves_a_quiet_session_with_no_signal() {
        assert_eq!(decide(&by_aterm(quiet_read())), Decision::Move);
        let mut r = quiet_read();
        r.screen.composer = Some("placeholder".to_string());
        r.screen.human = Human::Ago(Duration::from_secs(120));
        assert_eq!(
            decide(&by_aterm(r)),
            Decision::Move,
            "the boundary itself is quiet enough"
        );
    }

    #[test]
    fn every_condition_of_aterms_read_holds_the_move_on_its_own() {
        type Spoil = Box<dyn Fn(&mut AtermRead)>;
        let cases: Vec<(&str, Spoil)> = vec![
            ("claim", Box::new(|r| r.screen.proven = false)),
            (
                "claim",
                Box::new(|r| r.screen.supervisor = Some("codex-alab@9".to_string())),
            ),
            ("claim", Box::new(|r| r.holder = "grok-alab@10".to_string())),
            ("policy is off", Box::new(|r| r.policy.effective = false)),
            (
                "upgrade = false",
                Box::new(|r| r.policy.upgrade = Some(false)),
            ),
            ("no upgrade switch", Box::new(|r| r.policy.upgrade = None)),
            (
                "upgrade_ask = true",
                Box::new(|r| r.policy.upgrade_ask = Some(true)),
            ),
            ("no upgrade_ask", Box::new(|r| r.policy.upgrade_ask = None)),
            (
                "agent=busy",
                Box::new(|r| r.screen.agent = Some("busy".to_string())),
            ),
            (
                "agent=prompt",
                Box::new(|r| r.screen.agent = Some("prompt".to_string())),
            ),
            (
                "agent=unknown",
                Box::new(|r| r.screen.agent = Some("unknown".to_string())),
            ),
            (
                "agent=wall:usage",
                Box::new(|r| r.screen.agent = Some("wall:usage".to_string())),
            ),
            ("agent=absent", Box::new(|r| r.screen.agent = None)),
            (
                "composer=draft",
                Box::new(|r| r.screen.composer = Some("draft".to_string())),
            ),
            (
                "composer=-",
                Box::new(|r| r.screen.composer = Some("-".to_string())),
            ),
            ("no composer=", Box::new(|r| r.screen.composer = None)),
            (
                "typed 119999 ms ago",
                Box::new(|r| r.screen.human = Human::Ago(Duration::from_millis(119_999))),
            ),
            ("no human_ms", Box::new(|r| r.screen.human = Human::Unknown)),
            (
                "typed 89000 ms ago (the grace is 90000",
                Box::new(|r| {
                    r.policy.human_grace = Some(Duration::from_secs(90));
                    r.screen.human = Human::Ago(Duration::from_secs(89));
                }),
            ),
            (
                "hand=turn:7",
                Box::new(|r| r.screen.hand = Some("turn:7".to_string())),
            ),
            (
                "hold=1",
                Box::new(|r| r.screen.hold = Some("1".to_string())),
            ),
            (
                "input=typeahead",
                Box::new(|r| r.screen.input = Some("typeahead".to_string())),
            ),
            (
                "need=now",
                Box::new(|r| r.screen.need = Some("now".to_string())),
            ),
            ("need=absent", Box::new(|r| r.screen.need = None)),
        ];
        for (word, spoil) in cases {
            let mut r = quiet_read();
            spoil(&mut r);
            let why = held(&by_aterm(r));
            assert!(why.contains(word), "{word}: {why}");
        }
        let mut f = by_aterm(quiet_read());
        f.witness = Witness::Aterm(Err("aterm closed the connection".to_string()));
        assert!(held(&f).contains("no read of aterm"));
        // The local gates still stand under aterm's read.
        let mut f = by_aterm(quiet_read());
        f.descendants = Some(1);
        assert!(held(&f).contains("1 process"));
        let mut f = by_aterm(quiet_read());
        f.tty = Some(TtyQuiet {
            input: Duration::ZERO,
            output: Duration::from_secs(1),
        });
        assert!(held(&f).contains("wrote to the terminal"));
    }

    #[test]
    fn the_thresholds_are_atems_own() {
        assert_eq!(DEFAULT_QUIET, Duration::from_secs(20));
        assert_eq!(DEFAULT_HUMAN_GRACE, Duration::from_secs(120));
        let mut f = quiet_facts();
        f.tty = Some(TtyQuiet {
            input: DEFAULT_HUMAN_GRACE,
            output: DEFAULT_QUIET,
        });
        assert_eq!(
            decide(&f),
            Decision::Move,
            "the boundary itself is quiet enough"
        );
    }
}
