// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! grok-alab: the Grok Build harness for aterm.
//!
//! It runs xAI's UNMODIFIED `grok`, the build aterm's package manager installed and verified,
//! as its child on the inherited terminal, with every argument passed through. It never
//! ships, forks or patches vendor bytes, never finds the vendor on `PATH`, and its own
//! version is independent of the vendor's (`grok-alab 0.3.0 (grok 1.0.70)`).
//!
//! While it runs, atpkg may install a newer grok. At a safe point ([`safepoint`]) the harness
//! ends the child and relaunches it on the new build with the vendor's resume argv
//! ([`lanes`]), without exiting itself. When the session really ends, it ends with the
//! child's own status.
//!
//! Under aterm it holds the session under aterm's harness contract 3 ([`aterm`]): it claims
//! `relaunch,upgrade`, follows aterm's harness policy on its event (giving the claim back while
//! it is off), relaunches a crashed child on the same session on a fresh read of aterm, and
//! takes the safe point from aterm's read of the screen, a read that cannot pass on today's
//! aterm (it reports `agent=unknown` for every Grok session), so moves there still wait for
//! `SIGUSR1`. It never starts a grok that would start signed out itself ([`credentials`]).
//! Where aterm does not serve contract 3, or refuses the claim, it execs grok in its own place
//! and is gone ([`run`]).
//!
//! Its command line is `grok-alab --version`, and `grok-alab run [--] [grok arguments…]` (what
//! aterm's `grok` twin runs); `grok-alab [grok arguments…]` is `run` too. Arguments after `run`
//! are grok's, never the harness's own.
//!
//! Under aterm, a signed-out interactive launch first asks aterm to sign the machine in once
//! ([`signin`]), before it claims or passes through: either would start a grok that opens a
//! browser page of its own.

pub mod aterm;
pub mod credentials;
pub mod env;
pub mod journal;
pub mod lanes;
pub mod procs;
pub mod safepoint;
#[cfg(test)]
mod scratch;
pub mod session_id;
pub mod signals;
pub mod signin;
pub mod store;
pub mod supervise;

use std::ffi::OsString;
use std::path::Path;

use env::Env;
use supervise::exit_status;

/// The harness's name, as `--version` prints it.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// The harness's own version, independent of the vendor's: the version an atpkg pack signs
/// (`MAJOR.MINOR.0+g<sha9>`), which `atpkg-publish pack` hands the build as
/// `ALAB_HARNESS_VERSION` (aterm's harness design, D46), else the package's `MAJOR.MINOR.0`.
/// `--version`, the claim's `version=` and the journal read it. A value that is not this
/// package's line ([`is_pack_version`]) fails the build.
pub const VERSION: &str = match option_env!("ALAB_HARNESS_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

const _: () = assert!(
    is_pack_version(VERSION, env!("CARGO_PKG_VERSION")),
    "ALAB_HARNESS_VERSION is not this package's MAJOR.MINOR.0, alone or with +g<sha>"
);

const fn release_component(bytes: &[u8]) -> Option<&[u8]> {
    let (first, mut rest) = match bytes.split_first() {
        Some((&first, rest)) if first.is_ascii_digit() => (first, rest),
        _ => return None,
    };
    while let Some((&digit, tail)) = rest.split_first() {
        if digit == b'.' {
            return Some(tail);
        }
        if first == b'0' || !digit.is_ascii_digit() {
            return None;
        }
        rest = tail;
    }
    None
}

const fn release_base(bytes: &[u8]) -> bool {
    let Some(after_major) = release_component(bytes) else {
        return false;
    };
    let Some(after_minor) = release_component(after_major) else {
        return false;
    };
    matches!(after_minor, [b'0'])
}

/// Whether `v` is the `MAJOR.MINOR.0` release line `line` alone, or `line` followed by `+g` and
/// 9 to 40 lowercase hex digits (a dev build's commit): the only versions a pack may hand the
/// build.
#[must_use]
pub const fn is_pack_version(v: &str, line: &str) -> bool {
    let mut base = line.as_bytes();
    if !release_base(base) {
        return false;
    }
    let mut rest = v.as_bytes();
    while let Some((&expected, tail)) = base.split_first() {
        rest = match rest.split_first() {
            Some((&actual, tail)) if actual == expected => tail,
            _ => return false,
        };
        base = tail;
    }
    if rest.is_empty() {
        return true;
    }
    rest = match rest {
        [b'+', b'g', stamp @ ..] => stamp,
        _ => return false,
    };
    if !matches!(rest.len(), 9..=40) {
        return false;
    }
    while let Some((&digit, tail)) = rest.split_first() {
        if !matches!(digit, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
        rest = tail;
    }
    true
}

/// Whether `args` ask for the version: exactly `--version`, `-v` or `version`, the three
/// spellings Grok Build 1.0.70 answers with its own.
#[must_use]
pub fn is_version_request(args: &[OsString]) -> bool {
    matches!(args, [one] if one == "--version" || one == "-v" || one == "version")
}

/// What the harness's own command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    /// `grok-alab --version` (or `-v`, `version`): the harness's version line.
    Version,
    /// Run grok with these arguments.
    Run {
        /// grok's own arguments.
        vendor: Vec<OsString>,
        /// Whether they came after `run` (aterm's twin), so none is the harness's own.
        explicit: bool,
    },
}

/// Read the harness's command line: `run [--] <grok argv>`, a lone version word, or (the
/// v0.1.0 form) grok's argv itself. Grok Build has no `run` subcommand ([`lanes::COMMANDS`]),
/// so a first word `run` is always the harness's.
#[must_use]
pub fn cli(args: Vec<OsString>) -> Cli {
    if args.first().is_some_and(|a| a == "run") {
        // The words after `run`, less one leading `--`, read off an iterator: no index into the
        // vector, so nothing here can be out of bounds.
        let mut words = args.into_iter().skip(1).peekable();
        if words.peek().is_some_and(|a| a == "--") {
            words.next();
        }
        return Cli::Run {
            vendor: words.collect(),
            explicit: true,
        };
    }
    if is_version_request(&args) {
        return Cli::Version;
    }
    Cli::Run {
        vendor: args,
        explicit: false,
    }
}

/// What `--version` says when atpkg has no `current` build of grok.
pub const NOT_INSTALLED: &str = "not installed";

/// What `--version` says when grok's version cannot be read: a `current` that names no ready,
/// runnable build, or a `.vendor` record that is missing, unreadable or names another version
/// than its build.
pub const UNKNOWN: &str = "unknown";

/// `grok-alab <version> (grok <vendor version>)`: the harness's version and the version the
/// `.vendor` record of the build `current` names, when it names that build
/// ([`store::Build::version`]), else `(grok not installed)` or `(grok unknown)` ([`vendor_of`]).
/// `layout` is `None` when no store could be resolved. grok is never run for it, and a version
/// is never decoded from the build id alone.
#[must_use]
pub fn version_line(layout: Option<&store::Layout>) -> String {
    let current = layout.map(store::current);
    format!(
        "{NAME} {VERSION} ({} {})",
        store::COMMAND,
        vendor_of(current.as_ref())
    )
}

/// The vendor part of [`version_line`], from what the store said of its `current` build
/// (`None`: no store was resolved).
#[must_use]
pub fn vendor_of(current: Option<&Result<store::Build, store::Missing>>) -> &str {
    match current {
        Some(Ok(b)) => b.version.as_deref().unwrap_or(UNKNOWN),
        Some(Err(store::Missing::NotInstalled(_))) => NOT_INSTALLED,
        Some(Err(_)) | None => UNKNOWN,
    }
}

/// Whether `bin` is this very executable (a store entry that would make the harness exec
/// itself).
#[must_use]
pub fn is_self(bin: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(me) = std::env::current_exe() else {
        // Unknown: refuse rather than risk a loop.
        return true;
    };
    let same_path = match (std::fs::canonicalize(&me), std::fs::canonicalize(bin)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    let same_file = match (std::fs::metadata(&me), std::fs::metadata(bin)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    same_path || same_file
}

/// How many harnesses deep this one may run ([`supervise::DEPTH_ENV`]) before it refuses.
/// A person nesting a session inside a session's tool command goes one or two deep; a store
/// entry that runs this harness as its child goes on forever, and stops here.
pub const MAX_DEPTH: u32 = 8;

/// How deep this harness runs inside others, or why it must not run at all.
///
/// [`is_self`] catches a store entry that IS this executable; this catches one that LEADS
/// BACK to it, which [`is_self`] cannot see (a script or shim that runs `grok-alab`):
/// - its parent is the harness whose pid [`supervise::PID_ENV`] names: it was started in that
///   harness's vendor slot (the store's `grok` exec'd this harness), so running on would
///   start the same chain again, without end;
/// - [`supervise::DEPTH_ENV`] says [`MAX_DEPTH`] harnesses or more run above it (an entry that
///   runs this harness as its child), or is not a number.
///
/// `parent` is this process's parent pid.
///
/// # Errors
/// Why this harness must not run; it then exits 126 before running anything.
pub fn nesting(env: &Env, parent: u32) -> Result<u32, String> {
    let named = env
        .get(supervise::PID_ENV)
        .and_then(|v| v.to_str()?.trim().parse::<u32>().ok());
    if named == Some(parent) {
        return Err(format!(
            "started as the vendor of grok-alab pid {parent}: the store's {} leads back to this \
             harness; refusing to run it again",
            store::COMMAND
        ));
    }
    let depth = match env.get(supervise::DEPTH_ENV).filter(|v| !v.is_empty()) {
        None => 0,
        Some(v) => v
            .to_str()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .ok_or_else(|| format!("{} is not a number", supervise::DEPTH_ENV))?,
    };
    if depth >= MAX_DEPTH {
        return Err(format!(
            "{depth} harnesses already run above this one ({}): a store entry that runs this \
             harness would never end; refusing",
            supervise::DEPTH_ENV
        ));
    }
    Ok(depth)
}

/// The harness, from its arguments (after `argv[0]`) and environment to the wait status it
/// ends like ([`signals::end_like`]).
#[must_use]
pub fn run(args: Vec<OsString>, env: &Env) -> libc::c_int {
    let layout = store::resolve_prefix(env);
    let args = match cli(args) {
        Cli::Version => {
            // The harness's version is known whatever the store says: an unresolved store
            // makes the vendor's `unknown`, never a failed `--version`.
            println!("{}", version_line(layout.as_ref().ok()));
            return exit_status(0);
        }
        Cli::Run { vendor, .. } => vendor,
    };
    let layout = match layout {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return exit_status(2);
        }
    };
    let journal = journal::Journal::open(env);
    let depth = match nesting(env, std::os::unix::process::parent_id()) {
        Ok(d) => d,
        Err(why) => {
            eprintln!("{NAME}: {why}");
            journal.line("REFUSED", &why);
            return exit_status(126);
        }
    };
    let build = match store::current(&layout) {
        Ok(b) => b,
        Err(missing) => {
            eprintln!("{NAME}: {missing}");
            journal.line("REFUSED", &missing.to_string());
            return exit_status(127);
        }
    };
    if is_self(&build.bin) {
        let why = format!(
            "{} is this harness itself; refusing to run it as the vendor",
            build.bin.display()
        );
        eprintln!("{NAME}: {why}");
        journal.line("REFUSED", &why);
        return exit_status(126);
    }
    let plan = lanes::plan(&args, session_id::new_v4());
    if lanes::command(&plan) == Some(lanes::SELF_UPDATE_COMMAND) {
        let why = "`grok update` would replace the build aterm's package manager verified; \
                   run `aterm pkg update grok` instead";
        eprintln!("{NAME}: {why}");
        journal.line("REFUSED", why);
        return exit_status(2);
    }
    // Under aterm, a signed-out interactive launch is signed in once for the machine first
    // (Decision 26): the vendor's own sign-in would open a browser page per launch.
    let shim = store::shim_env(&layout, &build);
    let vendor_env = credentials::child_env(&shim, env);
    match signin::decide(signin::gated(&plan), &vendor_env, &mut signin::run_signin) {
        signin::Gate::Pass => {}
        signin::Gate::SignedIn => journal.line(
            "NOTE",
            "signed in to Grok Build through `aterm harness signin --cli grok`",
        ),
        signin::Gate::Refused(why) => {
            eprintln!("{NAME}: {why}");
            journal.line("REFUSED", &why);
            return exit_status(signin::EXIT_SIGNED_OUT);
        }
    }
    let claim = if aterm::under_aterm(env) {
        match under_aterm(env, &plan, &build) {
            Ok(claim) => {
                journal.line(
                    "CLAIMED",
                    &format!(
                        "{} holds the session under aterm contract {}, scope={} (aterm {})",
                        claim.holder,
                        aterm::REQUIRED_CONTRACT,
                        aterm::SCOPE,
                        claim.conn.version
                    ),
                );
                Some(claim)
            }
            Err(why) => return pass_through(&layout, &build, &args, depth, &journal, &why),
        }
    } else {
        None
    };
    let timing = supervise::Timing::from_env(env, &journal);
    let signals = match signals::Signals::install() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{NAME}: cannot install signal handlers: {e}");
            return exit_status(1);
        }
    };
    supervise::run(supervise::Session {
        layout: &layout,
        build,
        args,
        plan,
        journal: &journal,
        timing,
        signals: &signals,
        depth,
        claim,
    })
}

/// Under aterm: the start sequence of aterm's harness design §3.2, to a proven claim or the
/// reason to pass through. The person's `ATERM_NO_HARNESS=1` and a launch the harness cannot
/// carry pass through without a word to aterm.
///
/// # Errors
/// Why the harness gets out of the way.
pub fn under_aterm(
    env: &Env,
    plan: &lanes::Plan,
    build: &store::Build,
) -> Result<Box<aterm::Claimed>, String> {
    if aterm::left_out(env) {
        return Err(format!(
            "{}=1 leaves the harness out",
            aterm::NO_HARNESS_ENV
        ));
    }
    match plan {
        lanes::Plan::Session(_) => {}
        lanes::Plan::Uncarried { why } => {
            return Err(format!("this launch cannot be carried: {why}"));
        }
        lanes::Plan::PassThrough { why, .. } => {
            return Err(format!("not an interactive session: {why}"));
        }
    }
    let locator = aterm::Locator::from_env(env)?;
    match aterm::start(
        &locator,
        std::process::id(),
        VERSION,
        build.version.as_deref(),
        aterm::TIMEOUT,
    ) {
        aterm::Start::Claimed(c) => Ok(c),
        aterm::Start::PassThrough(why) => Err(why),
    }
}

/// Exec grok in this process's place with the launch's own arguments and the build's shim
/// environment: the tab then runs grok under its own name, as if no harness existed. Returns
/// only when the exec failed (127 when grok is missing, else 126).
#[must_use]
pub fn pass_through(
    layout: &store::Layout,
    build: &store::Build,
    args: &[OsString],
    depth: u32,
    journal: &journal::Journal,
    why: &str,
) -> libc::c_int {
    use std::os::unix::process::CommandExt;
    journal.line(
        "PASS",
        &format!(
            "grok {} runs in the harness's place (exec): {why}",
            build.version_label()
        ),
    );
    let mut cmd = std::process::Command::new(&build.bin);
    cmd.args(args);
    for (name, value) in store::shim_env(layout, build) {
        cmd.env(name, value);
    }
    // No harness pid: grok is not this harness's child. The depth still counts, so a store
    // entry that leads back here stops at MAX_DEPTH.
    cmd.env_remove(supervise::PID_ENV);
    cmd.env(supervise::DEPTH_ENV, depth.saturating_add(1).to_string());
    let e = cmd.exec();
    eprintln!("{NAME}: cannot run {}: {e}", build.bin.display());
    journal.line(
        "REFUSED",
        &format!("cannot exec {}: {e}", build.bin.display()),
    );
    exit_status(if e.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_lone_version_word_is_a_version_request() {
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        for yes in [&["--version"][..], &["-v"], &["version"]] {
            assert!(is_version_request(&os(yes)), "{yes:?}");
        }
        for no in [
            &[][..],
            &["--version", "x"],
            &["-V"],
            &["--model", "version"],
        ] {
            assert!(!is_version_request(&os(no)), "{no:?}");
        }
    }

    #[test]
    fn run_takes_every_word_after_it_as_groks() {
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        let run = |v: &[&str], explicit| Cli::Run {
            vendor: os(v),
            explicit,
        };
        assert_eq!(cli(os(&["--version"])), Cli::Version);
        assert_eq!(
            cli(os(&["run", "--", "--version"])),
            run(&["--version"], true)
        );
        assert_eq!(cli(os(&["run", "--version"])), run(&["--version"], true));
        assert_eq!(cli(os(&["run", "--", "--", "x"])), run(&["--", "x"], true));
        assert_eq!(cli(os(&["run"])), run(&[], true));
        assert_eq!(cli(os(&["run", "--"])), run(&[], true));
        assert_eq!(cli(os(&["--model", "m"])), run(&["--model", "m"], false));
        assert_eq!(cli(os(&[])), run(&[], false));
        assert_eq!(cli(os(&["x", "run"])), run(&["x", "run"], false));
        assert!(
            !lanes::COMMANDS.contains(&"run"),
            "grok has no `run` of its own"
        );
    }

    #[test]
    fn a_harness_started_in_its_own_vendor_slot_or_too_deep_refuses() {
        let env =
            |pairs: &'static [(&'static str, &'static str)]| Env::from_pairs(pairs.iter().copied());
        // A person's own launch, and a session nested in a session's tool command (the
        // parent is the tool's shell, not the outer harness).
        assert_eq!(nesting(&env(&[]), 77), Ok(0));
        assert_eq!(
            nesting(
                &env(&[(supervise::PID_ENV, "500"), (supervise::DEPTH_ENV, "1")]),
                77
            ),
            Ok(1)
        );
        // The store's grok exec'd this harness: its parent is the harness that ran it.
        let why = nesting(&env(&[(supervise::PID_ENV, "500")]), 500).unwrap_err();
        assert!(why.contains("leads back to this harness"), "{why}");
        // A chain that runs the harness as a child stops at MAX_DEPTH.
        assert_eq!(
            nesting(&env(&[(supervise::DEPTH_ENV, "7")]), 77),
            Ok(MAX_DEPTH - 1)
        );
        assert!(nesting(&env(&[(supervise::DEPTH_ENV, "8")]), 77).is_err());
        assert!(nesting(&env(&[(supervise::DEPTH_ENV, "x")]), 77).is_err());
        assert_eq!(nesting(&env(&[(supervise::DEPTH_ENV, "")]), 77), Ok(0));
    }

    #[test]
    fn only_a_missing_current_link_is_not_installed() {
        use std::path::PathBuf;
        let build = store::Build {
            id: 1_000_001_000_000_000_070,
            dir: PathBuf::from("/p/store/grok/1000001000000000070"),
            bin: PathBuf::from("/p/store/grok/1000001000000000070/bin/grok"),
            version: Some("1.0.70".to_string()),
        };
        assert_eq!(vendor_of(Some(&Ok(build.clone()))), "1.0.70");
        let unread = store::Build {
            version: None,
            ..build
        };
        assert_eq!(vendor_of(Some(&Ok(unread))), UNKNOWN);
        assert_eq!(
            vendor_of(Some(&Err(store::Missing::NotInstalled(PathBuf::from(
                "/p/store/grok/current"
            ))))),
            NOT_INSTALLED
        );
        for err in [
            store::Missing::BadLink(PathBuf::from("/p/store/grok/current"), "x".to_string()),
            store::Missing::NotReady(PathBuf::from("/p/store/grok/1.ready")),
            store::Missing::NoBinary(PathBuf::from("/p/store/grok/1/bin/grok")),
        ] {
            assert_eq!(vendor_of(Some(&Err(err.clone()))), UNKNOWN, "{err:?}");
        }
        assert_eq!(vendor_of(None), UNKNOWN, "no store resolved");
        assert_eq!(
            version_line(None),
            format!("grok-alab {VERSION} (grok unknown)")
        );
    }

    #[test]
    fn the_version_is_the_workspace_version_with_patch_zero() {
        assert_eq!(NAME, "grok-alab");
        let line = env!("CARGO_PKG_VERSION");
        let parts: Vec<&str> = line.split('.').collect();
        assert_eq!(parts.len(), 3, "{line}");
        assert_eq!(parts[2], "0", "the patch slot is always 0");
    }

    #[test]
    fn a_pack_hands_the_release_line_or_the_line_and_its_commit() {
        let line = "0.3.0";
        for (v, ok) in [
            ("0.3.0", true),
            ("0.3.0+gabcdef012", true),
            ("0.3.0+g0123456789abcdef0123456789abcdef01234567", true),
            ("0.3.0+gabcdef01", false),
            ("0.3.0+g0123456789abcdef0123456789abcdef012345678", false),
            ("0.3.0+gABCDEF012", false),
            ("0.3.0+gabcdef01z", false),
            ("0.3.0+abcdef012", false),
            ("0.3.0+g", false),
            ("0.3.0+", false),
            ("0.3.0-dev.3+gabcdef012", false),
            ("0.2.0+gabcdef012", false),
            ("0.3", false),
            ("", false),
        ] {
            assert_eq!(is_pack_version(v, line), ok, "{v:?}");
        }
        for bad_line in ["0.3.1", "01.3.0", "0.03.0", "0.3", "0.3.0.0", ""] {
            assert!(!is_pack_version(bad_line, bad_line), "{bad_line:?}");
            assert!(
                !is_pack_version(&format!("{bad_line}+gabcdef012"), bad_line),
                "{bad_line:?}"
            );
        }
        assert!(is_pack_version("10.20.0+gabcdef012", "10.20.0"));
        assert!(is_pack_version(VERSION, env!("CARGO_PKG_VERSION")));
        assert_eq!(
            VERSION,
            option_env!("ALAB_HARNESS_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        );
    }
}
