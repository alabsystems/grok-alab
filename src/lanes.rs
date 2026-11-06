// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GROK BUILD'S RESUME GRAMMAR: which launches the harness can carry onto a new build, and
//! the argv that carries them.
//!
//! WHERE IT COMES FROM.
//! - The resume pair is aterm's: `relaunch::resume_argv` gives Grok `--resume <id>`, and
//!   `relaunch::pool_line` puts every non-Codex CLI's resume pair AFTER the launch flags
//!   (aterm `crates/aterm-agent/src/harness/relaunch.rs`, `resume_argv` and `pool_line`, at
//!   origin/main 5e2c6d0c28). aterm has no relaunch lane for Grok (`lanes.rs`:
//!   `Program::Grok => Lanes::NONE`), so the rest is ported from the shape of its Muse Code
//!   lane (`crates/aterm-agent/src/harness/upgrade_muse.rs`: a table of root options, each with
//!   its arity and its fate on a relaunch; an unknown option refuses; the first prompt is
//!   never typed again).
//! - The option table is Grok Build 1.0.70's own `grok --help` (MEASURED 2026-10-24, xAI's
//!   darwin-arm64 build from registry.npmjs.org, run with a scratch `HOME`), plus the hidden
//!   `--dangerously-skip-permissions` aterm's vendor row knows (`table.rs`, `yolo_markers`).
//!   Its subcommands add 1.0.67's `cursor-worker`, `ssh`, `host-server`, `share` and
//!   `workspace` that aterm's row names (`yolo_skip`).
//!
//! HOW A SESSION IS NAMED. 1.0.70 resumes by `-r, --resume [<SESSION_ID_OR_TITLE>]` and
//! starts a new conversation under a chosen id with `-s, --session-id <SESSION_ID>`. A fresh
//! interactive launch therefore gets `--session-id <uuid>` put in front of its own arguments,
//! so the harness knows the id it will resume without reading the vendor's private files.
//! A launch that names its session by id (`--resume <uuid>`, `--session-id <uuid>`) keeps it.
//! A launch whose session the harness cannot name (`--continue`, `--resume` with no id or a
//! title, `--fork-session`) is run exactly as typed and never relaunched: fail closed.

use std::ffi::OsString;

/// How a root option takes its value (clap, as `grok --help` prints it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value: the next token whatever it looks like, or attached (`--model=x`).
    One,
    /// An optional value (`[<V>]`): attached, or the next token when it is no option.
    Optional,
}

/// What a relaunch does with a root option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Carried verbatim: the posture and setup the person launched with.
    Keep,
    /// Dropped with its value.
    Drop,
    /// The run prints or answers and exits: no interactive session to carry.
    OneShot,
    /// The session cannot be carried, for the reason given.
    Refuse(&'static str),
    /// `-r/--resume`: names the session to resume.
    Resume,
    /// `-s/--session-id`: names the new session.
    SessionId,
}

/// Grok Build 1.0.70's root options, each with its arity and its fate on a relaunch.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--add-dir", Arity::One, Fate::Keep),
    ("--agent", Arity::One, Fate::Keep),
    ("--agents", Arity::One, Fate::Keep),
    ("--allow", Arity::One, Fate::Keep),
    ("--allowedTools", Arity::One, Fate::Keep),
    ("--allowed-tools", Arity::One, Fate::Keep),
    ("--always-approve", Arity::None, Fate::Keep),
    (
        "-c",
        Arity::None,
        Fate::Refuse(
            "--continue resumes the folder's newest session, which a relaunch cannot name",
        ),
    ),
    (
        "--continue",
        Arity::None,
        Fate::Refuse(
            "--continue resumes the folder's newest session, which a relaunch cannot name",
        ),
    ),
    ("--cwd", Arity::One, Fate::Keep),
    ("--dangerously-skip-permissions", Arity::None, Fate::Keep),
    ("--debug", Arity::None, Fate::Keep),
    ("--debug-file", Arity::One, Fate::Keep),
    ("--deny", Arity::One, Fate::Keep),
    ("--disallowedTools", Arity::One, Fate::Keep),
    ("--disable-web-search", Arity::None, Fate::Keep),
    ("--disallowed-tools", Arity::One, Fate::Keep),
    (
        "--fork-session",
        Arity::None,
        Fate::Refuse("--fork-session gives the resumed session a new id the harness cannot name"),
    ),
    ("--fullscreen", Arity::None, Fate::Keep),
    ("-h", Arity::None, Fate::OneShot),
    ("--help", Arity::None, Fate::OneShot),
    ("--include-partial-messages", Arity::None, Fate::Keep),
    ("--json-schema", Arity::One, Fate::Keep),
    ("--leader-socket", Arity::One, Fate::Keep),
    ("-m", Arity::One, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("--max-turns", Arity::One, Fate::Keep),
    ("--minimal", Arity::None, Fate::Keep),
    ("--no-alt-screen", Arity::None, Fate::Keep),
    ("--no-plan", Arity::None, Fate::Keep),
    ("--no-subagents", Arity::None, Fate::Keep),
    ("--oauth", Arity::None, Fate::Keep),
    ("--output-format", Arity::One, Fate::Keep),
    ("-p", Arity::One, Fate::OneShot),
    ("--single", Arity::One, Fate::OneShot),
    ("--permission-mode", Arity::One, Fate::Keep),
    ("--prompt-file", Arity::One, Fate::OneShot),
    ("--prompt-json", Arity::One, Fate::OneShot),
    ("-r", Arity::Optional, Fate::Resume),
    ("--resume", Arity::Optional, Fate::Resume),
    ("--reasoning-effort", Arity::One, Fate::Keep),
    ("--effort", Arity::One, Fate::Keep),
    // Restores the original session's repository snapshot when resuming: typed again on a
    // relaunch it would put the repository back as it was, over the session's own work.
    ("--restore-code", Arity::None, Fate::Drop),
    ("--rules", Arity::One, Fate::Keep),
    ("-s", Arity::One, Fate::SessionId),
    ("--session-id", Arity::One, Fate::SessionId),
    ("--sandbox", Arity::One, Fate::Keep),
    ("--system-prompt-override", Arity::One, Fate::Keep),
    ("--system-prompt", Arity::One, Fate::Keep),
    ("--tools", Arity::One, Fate::Keep),
    ("-v", Arity::None, Fate::OneShot),
    ("--version", Arity::None, Fate::OneShot),
    // Applies to the first prompt, which a relaunch never sends again.
    ("--verbatim", Arity::None, Fate::Drop),
    (
        "-w",
        Arity::Optional,
        Fate::Refuse("--worktree starts the session in a new worktree a relaunch would not reopen"),
    ),
    (
        "--worktree",
        Arity::Optional,
        Fate::Refuse("--worktree starts the session in a new worktree a relaunch would not reopen"),
    ),
    (
        "--worktree-ref",
        Arity::One,
        Fate::Refuse(
            "--worktree-ref starts the session in a new worktree a relaunch would not reopen",
        ),
    ),
    (
        "--ref",
        Arity::One,
        Fate::Refuse("--ref starts the session in a new worktree a relaunch would not reopen"),
    ),
];

/// Grok Build's subcommands (1.0.70's `--help`, its aliases, and the 1.0.67 words aterm's
/// vendor row names). None runs the interactive session a relaunch brings back.
pub const COMMANDS: &[&str] = &[
    "agent",
    "clone",
    "completions",
    "cursor-worker",
    "dashboard",
    "disk-usage",
    "doctor",
    "du",
    "export",
    "help",
    "host-server",
    "inspect",
    "leader",
    "login",
    "logout",
    "mcp",
    "memory",
    "models",
    "plugin",
    "sessions",
    "setup",
    "share",
    "ssh",
    "trace",
    "update",
    "usage",
    "v",
    "version",
    "worker",
    "workspace",
    "worktree",
    "wrap",
];

/// The subcommand that runs the vendor's own updater (aterm's vendor row,
/// `self_update_verbs: &["update"]`).
pub const SELF_UPDATE_COMMAND: &str = "update";

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Whether clap reads `tok` as an option: a leading `-` and more after it.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// An option token's name and its attached value (`--model=x`). A short option is never read
/// attached (`-mX`, `-m=X`): no form the table knows, so it refuses as unknown.
fn option_name(tok: &str) -> (&str, Option<&str>) {
    match tok.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (tok, None),
    }
}

/// What the harness makes of one launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Not an interactive session (a subcommand, help, version, a one-shot prompt): run as
    /// typed, never relaunched.
    PassThrough {
        /// Why.
        why: String,
        /// The subcommand, when that is what makes it one.
        command: Option<String>,
    },
    /// An interactive session the harness cannot carry onto a new build: run as typed, never
    /// relaunched.
    Uncarried {
        /// Why.
        why: String,
    },
    /// An interactive session the harness can carry.
    Session(Carried),
}

/// An interactive session the harness can carry onto a new build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carried {
    /// The session id it resumes.
    pub session: String,
    /// The root options a relaunch types again, verbatim and in order.
    pub kept: Vec<String>,
    /// The `--session-id <id>` the harness put in front of the launch (empty when the launch
    /// named its own session).
    pub injected: Vec<String>,
}

impl Carried {
    /// The argv (after `argv[0]`) that resumes this session on a new build: the kept options,
    /// then the resume pair ([`resume_argv`]), as aterm's `pool_line` orders them.
    #[must_use]
    pub fn relaunch_argv(&self) -> Vec<String> {
        let mut out = self.kept.clone();
        out.extend(resume_argv(&self.session));
        out
    }
}

/// The pair that resumes session `id` of Grok Build (aterm `relaunch::resume_argv`,
/// `accounts::Cli::Grok => "--resume"`).
#[must_use]
pub fn resume_argv(id: &str) -> [String; 2] {
    ["--resume".to_string(), id.to_string()]
}

/// What a launch carried, read off its arguments.
#[derive(Debug, Default)]
struct Read {
    kept: Vec<String>,
    resume: Option<Option<String>>,
    session_id: Option<String>,
}

/// Read `args` (after `argv[0]`): `Ok` the session's parts, or `Err` the plan that says why
/// it is not one the harness carries.
fn read(args: &[String]) -> Result<Read, Plan> {
    let uncarried = |why: String| Plan::Uncarried { why };
    let mut out = Read::default();
    let mut positional_seen = false;
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_str();
        i += 1;
        if tok == "--" {
            // What follows is the first prompt, never typed again: one word, where no prompt
            // came before it and it could not be read as a subcommand.
            let rest = &args[i..];
            let plain = match rest {
                [] => true,
                [one] => !positional_seen && !COMMANDS.contains(&one.as_str()),
                _ => false,
            };
            if !plain {
                return Err(uncarried(
                    "the words after `--` are not one prompt".to_string(),
                ));
            }
            break;
        }
        if !maybe_option(tok) {
            // A subcommand word is the subcommand wherever it stands before `--`, even after
            // the prompt: clap 4 checks every token that is no option value against the
            // subcommands (clap_builder `parser.rs`, `get_matches_with`: the check is skipped
            // only while an option or a multi-value positional waits for its value, and
            // `possible_subcommand` ignores a positional already found unless
            // `args_conflicts_with_subcommands` is set, which grok's one-line usage
            // `grok [OPTIONS] [PROMPT] [COMMAND]` shows it is not). So `grok "hi" update` runs
            // the self-updater and must be refused like `grok update`.
            if COMMANDS.contains(&tok) {
                return Err(Plan::PassThrough {
                    why: format!("the `{tok}` subcommand"),
                    command: Some(tok.to_string()),
                });
            }
            if positional_seen {
                return Err(uncarried(format!("a second positional argument `{tok}`")));
            }
            positional_seen = true;
            // The first prompt: sent by the first launch, never again.
            continue;
        }
        let (name, attached) = option_name(tok);
        let Some((arity, fate)) = flag(name) else {
            return Err(uncarried(format!(
                "the option `{name}` is not in 1.0.70's --help"
            )));
        };
        let value: Option<String> = match (arity, attached) {
            (Arity::None, Some(_)) => {
                return Err(uncarried(format!("the switch `{name}` takes no value")));
            }
            (Arity::None, None) => None,
            (Arity::One | Arity::Optional, Some(v)) => Some(v.to_string()),
            (Arity::One, None) => {
                let Some(v) = args.get(i) else {
                    return Err(uncarried(format!("`{name}` has no value")));
                };
                i += 1;
                Some(v.clone())
            }
            (Arity::Optional, None) => match args.get(i) {
                Some(v) if !v.starts_with('-') => {
                    i += 1;
                    Some(v.clone())
                }
                _ => None,
            },
        };
        match fate {
            Fate::Keep => {
                out.kept.push(tok.to_string());
                if attached.is_none() {
                    out.kept.extend(value);
                }
            }
            Fate::Drop => {}
            Fate::OneShot => {
                return Err(Plan::PassThrough {
                    why: format!("`{name}` answers and exits"),
                    command: None,
                });
            }
            Fate::Refuse(why) => return Err(uncarried(why.to_string())),
            Fate::Resume => {
                if out.resume.is_some() || out.session_id.is_some() {
                    return Err(uncarried("two session options".to_string()));
                }
                out.resume = Some(value);
            }
            Fate::SessionId => {
                if out.resume.is_some() || out.session_id.is_some() {
                    return Err(uncarried("two session options".to_string()));
                }
                out.session_id = value;
            }
        }
    }
    Ok(out)
}

/// The plan for a launch with arguments `args` (after `argv[0]`). `fresh_id` is the id a fresh
/// interactive session is started under ([`crate::session_id::new_v4`]), made by the caller
/// before the plan is known and used only when the launch is such a session; `None` leaves that
/// launch uncarried.
///
/// A value, not a callback: the verified lane cannot hold a `pub fn` to a caller's
/// `impl FnOnce`, whose panic-freedom is false for every closure that panics (README,
/// "Verification").
#[must_use]
pub fn plan(args: &[OsString], fresh_id: Option<String>) -> Plan {
    let Some(args) = args
        .iter()
        .map(|a| a.to_str().map(str::to_string))
        .collect::<Option<Vec<String>>>()
    else {
        return Plan::Uncarried {
            why: "an argument is not UTF-8".to_string(),
        };
    };
    let read = match read(&args) {
        Ok(read) => read,
        Err(plan) => return plan,
    };
    let (session, injected) = match (read.resume, read.session_id) {
        (Some(Some(id)), None) if is_session_uuid(&id) => (id, Vec::new()),
        (Some(Some(_)), None) => {
            return Plan::Uncarried {
                why: "--resume names a title; a relaunch resumes by session id only".to_string(),
            };
        }
        (Some(None), None) => {
            return Plan::Uncarried {
                why: "--resume with no id opens the newest session, which a relaunch cannot name"
                    .to_string(),
            };
        }
        (None, Some(id)) if is_session_uuid(&id) => (id, Vec::new()),
        (None, Some(_)) => {
            return Plan::Uncarried {
                why: "--session-id is not a UUID".to_string(),
            };
        }
        (None, None) => {
            let Some(id) = fresh_id.filter(|id| is_session_uuid(id)) else {
                return Plan::Uncarried {
                    why: "no session id could be made".to_string(),
                };
            };
            (id.clone(), vec!["--session-id".to_string(), id])
        }
        (Some(_), Some(_)) => {
            return Plan::Uncarried {
                why: "two session options".to_string(),
            };
        }
    };
    Plan::Session(Carried {
        session,
        kept: read.kept,
        injected,
    })
}

/// The subcommand a launch runs, where the plan found one.
#[must_use]
pub fn command(plan: &Plan) -> Option<&str> {
    match plan {
        Plan::PassThrough {
            command: Some(c), ..
        } => Some(c),
        _ => None,
    }
}

/// Whether `s` is a hyphenated 8-4-4-4-12 hex UUID.
#[must_use]
pub fn is_session_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
