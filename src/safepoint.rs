// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SAFE POINT: when the harness may end its child to move it onto a newer build.
//!
//! The harness cannot see the screen (it inherits the terminal and proxies nothing), so it
//! cannot tell by itself that a turn has ended, the composer is empty and nobody is typing.
//! A move therefore needs a WITNESS for what only the screen shows ([`Witness`]):
//!
//! - **aterm's read** ([`Witness::Aterm`]), under a standing claim that takes `upgrade`
//!   ([`crate::aterm`]): one fresh `status` and `harness policy` pair judged by `alab-wire`'s
//!   safe-point rule (`alab_wire::claim::judge_safe_point`: the claim this harness's, proven
//!   and taking `upgrade`; the policy in effect with `upgrade=true` and `upgrade_ask=false`;
//!   `agent=idle`; `composer=empty` or `placeholder`; `human_ms` past the grace; `hand=-`,
//!   `hold=0`, `input=clear`, `need=-`), every field aterm does not report holding the move. No
//!   signal is needed: a newer build moves at the first read that passes.
//! - **a request** ([`Witness::Request`]): `SIGUSR1` to the harness, which says "the session is
//!   at a safe point now". Whoever sends it vouches for the screen; the terminal's input quiet
//!   is checked in place of aterm's read. Outside aterm it is the only witness; under a claim it
//!   stays as the person's override, and aterm's facts must still show the claim this
//!   harness's with `upgrade`, no halt and no other driver's hand
//!   ([`crate::aterm::unheld`]).
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
    /// A `SIGUSR1`: its sender vouches; the terminal's input quiet is checked. Under a claim,
    /// aterm's word on a person's move (`Err`: the fact that holds it, `aterm hold=1`), else
    /// `Ok`.
    Request(Result<(), String>),
    /// aterm's safe-point read under a standing claim with `upgrade`: `Ok` when it passed, else
    /// the hold (`wait aterm agent=unknown`) or why there was no read.
    Aterm(Result<(), String>),
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
        Witness::Request(word) => {
            if tty.input < f.human_grace {
                return hold(format!(
                    "someone typed {} ms ago (the grace is {} ms)",
                    tty.input.as_millis(),
                    f.human_grace.as_millis()
                ));
            }
            if let Err(why) = word {
                return hold(why.clone());
            }
        }
        Witness::Aterm(Err(why)) => return hold(why.clone()),
        Witness::Aterm(Ok(())) => {}
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
            witness: Witness::Request(Ok(())),
        }
    }

    fn by_aterm(word: Result<(), String>) -> Facts {
        let mut f = quiet_facts();
        // aterm's read replaces the terminal's input quiet: a terminal typed into a moment ago
        // (aterm's own continuation, say) does not hold it, aterm's human_ms does.
        f.tty = Some(TtyQuiet {
            input: Duration::ZERO,
            output: Duration::from_secs(600),
        });
        f.witness = Witness::Aterm(word);
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
    fn aterms_read_moves_a_quiet_session_with_no_signal_and_its_hold_holds() {
        assert_eq!(decide(&by_aterm(Ok(()))), Decision::Move);
        assert_eq!(
            held(&by_aterm(Err("wait aterm agent=unknown".to_string()))),
            "wait aterm agent=unknown"
        );
        // The local gates still stand under aterm's read.
        let mut f = by_aterm(Ok(()));
        f.descendants = Some(1);
        assert!(held(&f).contains("1 process"));
        let mut f = by_aterm(Ok(()));
        f.tty = Some(TtyQuiet {
            input: Duration::ZERO,
            output: Duration::from_secs(1),
        });
        assert!(held(&f).contains("wrote to the terminal"));
    }

    /// A person's request under a claim: aterm's word on it holds the move after the terminal's
    /// input quiet, and a request that fails the local gates is refused for those first.
    #[test]
    fn aterms_word_on_a_request_holds_it_after_the_local_gates() {
        let mut f = quiet_facts();
        f.witness = Witness::Request(Err("aterm hold=1".to_string()));
        assert_eq!(held(&f), "aterm hold=1");
        f.tty = Some(TtyQuiet {
            input: Duration::from_secs(1),
            output: Duration::from_secs(600),
        });
        assert!(held(&f).contains("someone typed"), "{f:?}");
        f.descendants = Some(1);
        assert!(held(&f).contains("1 process"), "{f:?}");
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
