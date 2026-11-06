// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SIGN-IN GATE UNDER ATERM (Decision 26).
//!
//! A signed-out Grok Build opens xAI's sign-in page in the desk's default browser on every
//! interactive launch, through macOS's LaunchServices whatever its `PATH` (aterm
//! `docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md` §6). Under aterm, ten sessions starting at
//! once would open ten pages and start ten device flows. aterm owns the fix: `aterm harness
//! signin --cli grok --wait` signs the machine in once, inside a sandbox that stops the vendor's
//! browser open, publishes one approval need with the code and the page, and makes every other
//! caller wait for that one sign-in.
//!
//! This module only asks aterm for it. Before it spawns an interactive child under aterm (the
//! first one, and every relaunch: [`crate::supervise`]), the harness reads the vendor's sign-in
//! state the way the vendor does, from its environment and files alone
//! ([`crate::credentials::signed_in`], which also reads aterm's refusal marker), and
//! when that says signed out it runs the aterm verb in the foreground and starts `grok` only
//! once that exits 0. Otherwise a first start exits [`EXIT_SIGNED_OUT`] (69, `EX_UNAVAILABLE`)
//! with one line, and a relaunch ends the session with its resume hint; `grok` never runs. A
//! launch is gated unless it is a one-shot option or one of
//! [`LOCAL_COMMANDS`] ([`gated`]). This is not a claim on the session (no supervision, nothing
//! typed; see [`crate::aterm`]), so it needs no contract 3.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::env::Env;

/// The exit status of a launch the gate stopped: `EX_UNAVAILABLE`, as aterm's twin uses.
pub const EXIT_SIGNED_OUT: u8 = 69;

/// The aterm verb's words after `aterm`.
pub const SIGNIN_ARGS: [&str; 5] = ["harness", "signin", "--cli", "grok", "--wait"];

/// The subcommands that start neither grok's interactive screen nor a sign-in of its own, as
/// aterm's grok twin names them (atpkg's `SigninGate::local`): a local read or edit, help,
/// version, and grok's own `login` and `logout`, which a person types to sign in or out. Every
/// other subcommand is gated (`dashboard` opens the interactive screen, `sessions` restores one,
/// and an unknown word may), so a signed-out one is signed in first, never run to open a page.
pub const LOCAL_COMMANDS: &[&str] = &[
    "login",
    "logout",
    "help",
    "version",
    "v",
    "completions",
    "doctor",
    "du",
    "disk-usage",
    "inspect",
    "export",
    "usage",
    "mcp",
    "memory",
    "leader",
    "worktree",
    "trace",
];

/// Whether a launch with this plan is gated: every launch but a one-shot option (`--help`,
/// `--version`, `-p` and the other one-prompt options) and a subcommand of [`LOCAL_COMMANDS`].
#[must_use]
pub fn gated(plan: &crate::lanes::Plan) -> bool {
    match plan {
        crate::lanes::Plan::PassThrough {
            command: Some(c), ..
        } => !LOCAL_COMMANDS.contains(&c.as_str()),
        crate::lanes::Plan::PassThrough { command: None, .. } => false,
        crate::lanes::Plan::Uncarried { .. } | crate::lanes::Plan::Session(_) => true,
    }
}

/// Whether the gate applies: under aterm (`ATERM_CHILD=1`, the variable aterm sets for every
/// program it starts), and not taken away by `--no-harness` (`ATERM_NO_HARNESS`, which
/// atpkg's twin sets for it).
#[must_use]
pub fn applies(env: &Env) -> bool {
    env.get("ATERM_CHILD").is_some_and(|v| v == "1")
        && env.get("ATERM_NO_HARNESS").is_none_or(|v| v.is_empty())
}

/// The `aterm` the gate runs: the first executable `aterm` in an absolute `PATH` entry (a
/// relative entry would resolve against whatever directory the launch is in, so it is skipped).
#[must_use]
pub fn find_aterm(env: &Env) -> Option<PathBuf> {
    let path = env.get("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join("aterm"))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// What the gate decided for one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Not under aterm, not interactive, or already signed in: run as before.
    Pass,
    /// aterm signed the machine in (its verb exited 0): run.
    SignedIn,
    /// Do not run `grok`: exit [`EXIT_SIGNED_OUT`] with this line.
    Refused(String),
}

/// The gate for one start of `grok`: `interactive` is whether the start is gated ([`gated`];
/// a relaunch always is), `env` is the environment the vendor would start with, and
/// `signin` runs the aterm verb at the path given (inheriting the terminal) and returns whether
/// it exited 0, or why it could not run.
pub fn decide(
    interactive: bool,
    env: &Env,
    signin: &mut impl FnMut(&Path) -> Result<bool, String>,
) -> Gate {
    if !interactive || !applies(env) || crate::credentials::signed_in(env) {
        return Gate::Pass;
    }
    let Some(aterm) = find_aterm(env) else {
        return Gate::Refused(
            "signed out of Grok Build, and aterm (which signs this machine in once) is not on \
             PATH; grok was not started"
                .to_string(),
        );
    };
    match signin(&aterm) {
        Ok(true) => Gate::SignedIn,
        Ok(false) => Gate::Refused(
            "not signed in to Grok Build (`aterm harness signin --cli grok` says why above); \
             grok was not started"
                .to_string(),
        ),
        Err(why) => Gate::Refused(format!(
            "cannot run `{} harness signin --cli grok --wait`: {why}; grok was not started",
            aterm.display()
        )),
    }
}

/// Runs `aterm harness signin --cli grok --wait` in the foreground on the inherited terminal
/// and environment, and returns whether it exited 0.
///
/// # Errors
/// The verb could not be started.
pub fn run_signin(aterm: &Path) -> Result<bool, String> {
    std::process::Command::new(aterm)
        .args(SIGNIN_ARGS)
        .status()
        .map(|s| s.success())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::ffi::OsString;

    /// A scratch directory, removed when the test ends, passed or failed.
    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("signin-{name}"))
    }

    fn env(pairs: Vec<(&'static str, OsString)>) -> Env {
        Env::from_pairs(pairs)
    }

    #[test]
    fn only_a_one_shot_option_or_a_local_subcommand_skips_the_gate() {
        let plan = |args: &[&str]| {
            let args: Vec<OsString> = args.iter().map(OsString::from).collect();
            crate::lanes::plan(
                &args,
                Some("00000000-0000-4000-8000-000000000000".to_string()),
            )
        };
        for args in [
            &[][..],
            &["fix it"][..],
            &["--resume", "x"][..],
            &["dashboard"][..],
            &["sessions"][..],
            &["models"][..],
            &["agent", "stdio"][..],
            &["fix it", "dashboard"][..],
        ] {
            assert!(gated(&plan(args)), "{args:?}: {:?}", plan(args));
        }
        for args in [
            &["login"][..],
            &["logout"][..],
            &["--help"][..],
            &["--version"][..],
            &["-p", "hi"][..],
            &["mcp", "list"][..],
            &["doctor"][..],
        ] {
            assert!(!gated(&plan(args)), "{args:?}: {:?}", plan(args));
        }
    }

    #[test]
    fn the_gate_asks_aterm_only_for_a_signed_out_interactive_launch_under_aterm() {
        let d = scratch("gate");
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let aterm = bin.join("aterm");
        std::fs::write(&aterm, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = || {
            (
                "PATH",
                OsString::from(format!("relative:{}", bin.display())),
            )
        };
        let child = || ("ATERM_CHILD", OsString::from("1"));
        let asked = std::cell::RefCell::new(Vec::new());
        let answer = |ok: Result<bool, String>| {
            let want = aterm.clone();
            move |p: &Path| {
                assert_eq!(p, want.as_path());
                ok.clone()
            }
        };
        // Signed out, interactive, under aterm: aterm decides.
        let get = env(vec![path(), child()]);
        assert_eq!(decide(true, &get, &mut answer(Ok(true))), Gate::SignedIn);
        let Gate::Refused(why) = decide(true, &get, &mut answer(Ok(false))) else {
            panic!("a failed sign-in must refuse");
        };
        assert!(why.contains("grok was not started"), "{why}");
        assert!(matches!(
            decide(true, &get, &mut answer(Err("denied".into()))),
            Gate::Refused(w) if w.contains("denied")
        ));
        // Every other launch passes without asking.
        let mut never = |p: &Path| -> Result<bool, String> {
            asked.borrow_mut().push(p.to_path_buf());
            Ok(false)
        };
        assert_eq!(decide(false, &get, &mut never), Gate::Pass);
        assert_eq!(decide(true, &env(vec![path()]), &mut never), Gate::Pass);
        assert_eq!(
            decide(
                true,
                &env(vec![path(), ("ATERM_CHILD", "0".into())]),
                &mut never
            ),
            Gate::Pass
        );
        assert_eq!(
            decide(
                true,
                &env(vec![path(), child(), ("ATERM_NO_HARNESS", "1".into())]),
                &mut never
            ),
            Gate::Pass
        );
        assert_eq!(
            decide(
                true,
                &env(vec![path(), child(), ("XAI_API_KEY", "k".into())]),
                &mut never
            ),
            Gate::Pass
        );
        // No aterm on PATH (and a relative entry is never searched): refused, nothing asked.
        let Gate::Refused(why) = decide(
            true,
            &env(vec![("PATH", "relative:/nonexistent".into()), child()]),
            &mut never,
        ) else {
            panic!("no aterm must refuse");
        };
        assert!(why.contains("not on PATH"), "{why}");
        assert!(asked.borrow().is_empty(), "{:?}", asked.borrow());
    }
}
