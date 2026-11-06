// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE VENDOR BINARY, FROM ATERM'S PACKAGE STORE AND NOWHERE ELSE.
//!
//! atpkg installs Grok Build as the vendor-direct program `grok` (aterm
//! `crates/atpkg/src/vendor_direct/table.rs`, the `program: "grok"` row: xAI's own build from
//! registry.npmjs.org, checked against storage.googleapis.com/grok-build-public-artifacts).
//! Its layout (aterm `crates/atpkg/src/store.rs`, `Layout::program_current` and
//! `crates/aterm-toolchain/src/layout.rs`, `build_dir`):
//!
//! ```text
//! <prefix>/store/grok/<build>/bin/grok    the build, immutable
//! <prefix>/store/grok/<build>.ready       "ok" first, written before the flip
//! <prefix>/store/grok/<build>.vendor      the verified record (`version = "1.0.70"`)
//! <prefix>/store/grok/<build>.shim-env    NAME=VALUE lines aterm runs the build with
//! <prefix>/store/grok/current -> <build>  the build atpkg made current
//! ```
//!
//! `<prefix>` is `~/Library/Application Support/aterm/pkg` on macOS and
//! `~/.local/share/aterm/pkg` elsewhere (`crates/atpkg/src/platform/unix.rs`,
//! `default_prefix`), unless `[packages] prefix` in `aterm.toml` names another.
//!
//! The harness never looks the vendor up on `PATH` and never through atpkg's `agents/` twin
//! or `bin/` shim: those may be (or later exec) this harness, so a `PATH` lookup could exec
//! itself. It execs the store build's own `bin/grok`, as the twin does.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use nix::unistd::{User, getuid};

use crate::env::Env;

/// atpkg's program name for Grok Build (the vendor row's `program`).
pub const PROGRAM: &str = "grok";

/// The command the vendor build carries in `bin/` (the vendor row's `exposes`).
pub const COMMAND: &str = "grok";

/// The variable that overrides the package prefix (an absolute path).
pub const PREFIX_ENV: &str = "GROK_ALAB_PKG_PREFIX";

/// The vendor's self-update switch and the value that turns it off. aterm's vendor row runs
/// every managed grok with it (`shim_env: &["GROK_DISABLE_AUTOUPDATER=1"]`), and atpkg lists
/// it among the self-update switches (`crates/atpkg/src/shim_env.rs`,
/// `SELF_UPDATE_SWITCHES`). The harness sets it even when a build's sidecar is missing: the
/// build that runs is the one atpkg verified, never one the vendor fetched for itself.
pub const AUTOUPDATER_OFF: (&str, &str) = ("GROK_DISABLE_AUTOUPDATER", "1");

/// Bound for reading a sidecar or the config file.
const MAX_SIDECAR_BYTES: u64 = 4 * 1024;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// The most entries a shim environment may declare (atpkg `MAX_SHIM_ENV`).
const MAX_SHIM_ENV: usize = 8;
/// The longest `NAME=VALUE` entry, in bytes (atpkg `MAX_ENTRY_BYTES`).
const MAX_ENTRY_BYTES: usize = 256;
/// Names a shim never sets (atpkg `NEVER_SET`).
const NEVER_SET: &[&str] = &[
    "PATH",
    "HOME",
    "IFS",
    "ENV",
    "BASH_ENV",
    "SHELLOPTS",
    "SHELL",
    "PWD",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "PATHEXT",
    "COMSPEC",
    "SYSTEMROOT",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
];
/// Name prefixes a shim never sets (atpkg `NEVER_SET_PREFIXES`).
const NEVER_SET_PREFIXES: &[&str] = &["LD_", "DYLD_"];

/// The resolved package prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The prefix, e.g. `~/Library/Application Support/aterm/pkg`.
    pub prefix: PathBuf,
}

impl Layout {
    /// `<prefix>/store/grok`.
    #[must_use]
    pub fn program_dir(&self) -> PathBuf {
        self.prefix.join("store").join(PROGRAM)
    }

    /// `<prefix>/store/grok/current`.
    #[must_use]
    pub fn current_link(&self) -> PathBuf {
        self.program_dir().join("current")
    }
}

/// One installed, ready vendor build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// Its store build id (vendor builds encode their version, so a newer one is larger).
    pub id: u64,
    /// `<prefix>/store/grok/<build>`.
    pub dir: PathBuf,
    /// `<prefix>/store/grok/<build>/bin/grok`, the file the harness execs.
    pub bin: PathBuf,
    /// The vendor version the build's `.vendor` record names, only when it names this very
    /// build (the version the id encodes, [`record_names_build`]); `None` when the record is
    /// missing, unreadable or names another version. Never decoded from the id alone.
    pub version: Option<String>,
}

impl Build {
    /// The version for display (`1.0.70`, or `build <id>` when none is known).
    #[must_use]
    pub fn version_label(&self) -> String {
        self.version
            .clone()
            .unwrap_or_else(|| format!("build {}", self.id))
    }
}

/// Why the store names no usable vendor build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// No `current` link: grok is not installed.
    NotInstalled(PathBuf),
    /// The link is unreadable, or names something that is not a build of this store.
    BadLink(PathBuf, String),
    /// The build has no `.ready` marker that says `ok`.
    NotReady(PathBuf),
    /// The build carries no executable `bin/grok`.
    NoBinary(PathBuf),
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Missing::NotInstalled(link) => write!(
                f,
                "grok is not installed in aterm's package store ({} does not exist); install it with `aterm pkg install grok`",
                link.display()
            ),
            Missing::BadLink(link, why) => {
                write!(f, "{} does not name a grok build: {why}", link.display())
            }
            Missing::NotReady(marker) => write!(
                f,
                "the current grok build is not marked ready ({} does not say ok); let `aterm pkg update` finish",
                marker.display()
            ),
            Missing::NoBinary(bin) => write!(
                f,
                "the current grok build has no executable {}",
                bin.display()
            ),
        }
    }
}

/// A build directory's name as the store writes one: digits, no leading zero, not zero
/// (aterm `crates/aterm-toolchain/src/layout.rs`, `plain_build_name`). A build laid aside
/// as `<build>.noindex` is read as `<build>`.
#[must_use]
pub fn plain_build_name(name: &str) -> Option<u64> {
    let name = name.strip_suffix(".noindex").unwrap_or(name);
    let digits = !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit());
    if !digits || (name.len() > 1 && name.starts_with('0')) {
        return None;
    }
    name.parse::<u64>().ok().filter(|b| *b != 0)
}

/// The build `current` names, ready and runnable.
///
/// # Errors
/// [`Missing`] says why there is none.
pub fn current(layout: &Layout) -> Result<Build, Missing> {
    let link = layout.current_link();
    let target = match fs::read_link(&link) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Missing::NotInstalled(link));
        }
        Err(e) => return Err(Missing::BadLink(link, e.to_string())),
    };
    let program_dir = layout.program_dir();
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Missing::BadLink(link.clone(), "no build name".to_string()))?;
    let id = plain_build_name(name)
        .ok_or_else(|| Missing::BadLink(link.clone(), format!("`{name}` is no build id")))?;
    // Absolute or relative, the link must name a build of this very store.
    let parent_ok = match target.parent() {
        None => true,
        Some(p) if p.as_os_str().is_empty() => true,
        Some(p) if target.is_absolute() => p == program_dir,
        Some(_) => false,
    };
    if !parent_ok {
        return Err(Missing::BadLink(
            link,
            format!("{} is outside {}", target.display(), program_dir.display()),
        ));
    }
    build(layout, id)
}

/// Whether a `.ready` marker admits its build on `running` (`store.rs` `ready_text_accepts`):
/// the first line is `ok`, and any `platform=` line matches `running` (`<arch>-<os>`).
#[must_use]
pub fn ready_text_accepts(text: &str, running: &str) -> bool {
    if text.lines().next().map(str::trim) != Some("ok") {
        return false;
    }
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("platform="))
        .map(str::trim)
        .find(|p| !p.is_empty())
        .is_none_or(|p| p == running)
}

/// Build `id` of the store, ready and runnable.
///
/// # Errors
/// [`Missing`] says why it is not.
pub fn build(layout: &Layout, id: u64) -> Result<Build, Missing> {
    let program_dir = layout.program_dir();
    let dir = program_dir.join(id.to_string());
    let ready = program_dir.join(format!("{id}.ready"));
    let running = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let says_ok = fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir())
        && read_sidecar(&ready, MAX_SIDECAR_BYTES)
            .is_some_and(|text| ready_text_accepts(&text, &running));
    if !says_ok {
        return Err(Missing::NotReady(ready));
    }
    let bin_dir = dir.join("bin");
    let bin = bin_dir.join(COMMAND);
    if !fs::symlink_metadata(&bin_dir).is_ok_and(|m| m.is_dir()) || !is_executable_file(&bin) {
        return Err(Missing::NoBinary(bin));
    }
    let version = read_sidecar(&program_dir.join(format!("{id}.vendor")), MAX_SIDECAR_BYTES)
        .and_then(|text| vendor_version(&text))
        .filter(|v| record_names_build(v, id));
    Ok(Build {
        id,
        dir,
        bin,
        version,
    })
}

/// The environment aterm runs `build` with (its `.shim-env` sidecar, admitted under atpkg's
/// rule, fail-closed to none), with the vendor's self-update switch always off
/// ([`AUTOUPDATER_OFF`]).
#[must_use]
pub fn shim_env(layout: &Layout, build: &Build) -> Vec<(String, String)> {
    let path = layout.program_dir().join(format!("{}.shim-env", build.id));
    let sidecar = read_sidecar(&path, MAX_SIDECAR_BYTES)
        .map(|text| admit_shim_env(&text))
        .unwrap_or_default();
    with_autoupdater_off(sidecar)
}

/// `env` with the vendor's self-update switch set to off, whatever value (if any) it carried:
/// a sidecar that says `GROK_DISABLE_AUTOUPDATER=0` never turns the updater back on.
#[must_use]
pub fn with_autoupdater_off(mut env: Vec<(String, String)>) -> Vec<(String, String)> {
    env.retain(|(n, _)| n != AUTOUPDATER_OFF.0);
    env.push((AUTOUPDATER_OFF.0.to_string(), AUTOUPDATER_OFF.1.to_string()));
    env
}

/// The `NAME=VALUE` lines of a shim-env sidecar, or none at all when any line breaks atpkg's
/// rule (`crates/atpkg/src/shim_env.rs`, `ShimEnv::from_lines`: fail-closed to no env).
#[must_use]
pub fn admit_shim_env(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if lines.len() > MAX_SHIM_ENV {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::with_capacity(lines.len());
    for line in lines {
        let Some((name, value)) = admit_entry(line) else {
            return Vec::new();
        };
        if out.iter().any(|(n, _)| n == name) {
            return Vec::new();
        }
        out.push((name.to_string(), value.to_string()));
    }
    out
}

fn admit_entry(entry: &str) -> Option<(&str, &str)> {
    if entry.len() > MAX_ENTRY_BYTES {
        return None;
    }
    let (name, value) = entry.split_once('=')?;
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
        && !NEVER_SET.contains(&name)
        && !NEVER_SET_PREFIXES.iter().any(|p| name.starts_with(p));
    let value_ok = !value.is_empty()
        && !value.chars().any(char::is_control)
        && value.trim() == value
        && !value.contains('"')
        && !value.contains('%');
    (name_ok && value_ok).then_some((name, value))
}

/// `version = "<v>"` of a `.vendor` record, where `<v>` is plain version text.
#[must_use]
pub fn vendor_version(record: &str) -> Option<String> {
    record.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("version")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        let v = rest.strip_prefix('"')?.strip_suffix('"')?;
        let plain = !v.is_empty()
            && v.len() <= 64
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".+-_".contains(&b));
        plain.then(|| v.to_string())
    })
}

/// Whether a `.vendor` record's version `recorded` names build `id`: it is exactly the version
/// the id encodes ([`decode_vendor_build`]), so the record and the directory agree before either
/// is believed (aterm's store reads a build the same way). The id is decoded only to check the
/// record, never to stand in for one.
#[must_use]
pub fn record_names_build(recorded: &str, id: u64) -> bool {
    decode_vendor_build(id).as_deref() == Some(recorded)
}

/// The vendor version a store build id encodes (aterm
/// `crates/atpkg/src/vendor_direct/version.rs`): `x.y.z` is `10^18 + x·10^12 + y·10^6 + z`,
/// and `x.y.z-R<n>[.<s>]` is `2·10^18 + x·10^15 + y·10^12 + z·10^9 + n·10^3 + (s+1 or 0)`.
/// `None` for an ALab build number (below `10^18`) or past the vendor tiers.
#[must_use]
pub fn decode_vendor_build(id: u64) -> Option<String> {
    const BASE: u64 = 1_000_000_000_000_000_000;
    match id / BASE {
        1 => {
            let v = id - BASE;
            let (x, y, z) = (
                v / 1_000_000_000_000,
                v / 1_000_000 % 1_000_000,
                v % 1_000_000,
            );
            Some(format!("{x}.{y}.{z}"))
        }
        2 => {
            let v = id - 2 * BASE;
            let x = v / 1_000_000_000_000_000 % 1_000;
            let y = v / 1_000_000_000_000 % 1_000;
            let z = v / 1_000_000_000 % 1_000;
            let n = v / 1_000 % 1_000_000;
            let s = v % 1_000;
            Some(match s {
                0 => format!("{x}.{y}.{z}-R{n}"),
                s => format!("{x}.{y}.{z}-R{n}.{}", s - 1),
            })
        }
        _ => None,
    }
}

/// Resolve the package prefix: [`PREFIX_ENV`] when set, else `[packages] prefix` in
/// `aterm.toml` when it names an existing absolute directory, else atpkg's default.
/// `env` is the environment it reads (a seam for tests).
///
/// # Errors
/// [`PREFIX_ENV`] is set but not absolute, or no home directory is known.
pub fn resolve_prefix(env: &Env) -> Result<Layout, String> {
    if let Some(v) = env.get(PREFIX_ENV).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if !plain_absolute(&p) {
            return Err(format!(
                "{PREFIX_ENV} must be an absolute path with no `..` (it is {})",
                p.display()
            ));
        }
        return Ok(Layout { prefix: p });
    }
    let home = env
        .get("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .or_else(account_home)
        .ok_or_else(|| {
            "no home directory is known (HOME is unset, empty or not absolute, and the password \
             database names no absolute home for this user)"
                .to_string()
        })?;
    let config = env
        .get("XDG_CONFIG_HOME")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .filter(|x| x.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("aterm")
        .join("aterm.toml");
    if let Some(text) = read_bounded(&config, MAX_CONFIG_BYTES)
        && let Some(value) = config_prefix(&text)
        && let Some(p) = expand_home(&value, &home)
        && plain_absolute(&p)
        && p.is_dir()
    {
        return Ok(Layout { prefix: p });
    }
    Ok(Layout {
        prefix: default_prefix(&home),
    })
}

/// atpkg's default prefix under `home` (`crates/atpkg/src/platform/unix.rs`).
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    } else {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// `prefix` of the `[packages]` table (or a root `packages.prefix`) of an `aterm.toml`, when
/// it is a plain quoted string on one line. Anything this reader cannot read plainly (an
/// escape, an inline table) is no answer, and the default prefix stands.
#[must_use]
pub fn config_prefix(text: &str) -> Option<String> {
    let mut table: Option<String> = Some(String::new());
    let mut multi: Option<&'static str> = None;
    let mut found = None;
    for raw in text.lines() {
        if let Some(delim) = multi {
            if raw.matches(delim).count() % 2 == 1 {
                multi = None;
            }
            continue;
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let opens = ["\"\"\"", "'''"]
            .into_iter()
            .find(|d| line.matches(d).count() % 2 == 1);
        if line.starts_with("[[") {
            table = None;
            multi = opens;
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            table = rest
                .split_once(']')
                .map(|(name, _)| name.trim().trim_matches('"').to_string());
            multi = opens;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            multi = opens;
            continue;
        };
        let key = key.trim();
        let wanted = match table.as_deref() {
            Some("packages") => key == "prefix",
            Some("") => key == "packages.prefix",
            _ => false,
        };
        if wanted {
            found = if opens.is_some() {
                None
            } else {
                quoted(value.trim())
            };
        }
        multi = opens;
    }
    found
}

/// The text of a one-line TOML basic (no escapes) or literal string, trailing comment allowed.
fn quoted(value: &str) -> Option<String> {
    if value.starts_with("\"\"\"") || value.starts_with("'''") {
        return None;
    }
    let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &value[1..];
    let end = body.find(quote)?;
    let (text, after) = (&body[..end], body[end + 1..].trim());
    if (quote == '"' && text.contains('\\')) || !(after.is_empty() || after.starts_with('#')) {
        return None;
    }
    Some(text.to_string())
}

fn expand_home(value: &str, home: &Path) -> Option<PathBuf> {
    match value.strip_prefix('~') {
        Some("") => Some(home.to_path_buf()),
        Some(rest) => rest.strip_prefix('/').map(|tail| home.join(tail)),
        None => Some(PathBuf::from(value)),
    }
}

fn plain_absolute(p: &Path) -> bool {
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// The account's home from the password database, for a process whose `HOME` is unset, empty
/// or not absolute ([`resolve_prefix`]), as v0.1.0 to v0.3.0 read it: nix's
/// [`User::from_uid`] of the real uid is `getpwuid_r(3)` behind a safe call (it sizes its
/// buffer from `sysconf` and retries larger, up to 1 MiB). Not [`std::env::home_dir`]: that
/// reads the database only when `HOME` is unset or empty and hands back a relative `HOME` as
/// it is, so a relative `HOME` found no home at all and `run` exited 2 before reading its
/// arguments. No entry, a failed lookup (its error is dropped on purpose, as v0.1.0 dropped
/// `getpwuid_r`'s: the caller reports that no home is known) or a relative home in the
/// database is no home.
fn account_home() -> Option<PathBuf> {
    match User::from_uid(getuid()) {
        Ok(Some(user)) if user.dir.is_absolute() => Some(user.dir),
        Ok(_) | Err(_) => None,
    }
}

/// Whether `path` (followed) is a regular file with an execute bit.
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The UTF-8 text of `path`, followed, when it is a regular file of at most `max` bytes.
fn read_bounded(path: &Path, max: u64) -> Option<String> {
    read_bounded_opts(path, max, true)
}

/// The UTF-8 text of a store sidecar `path` (never a symlink) of at most `max` bytes.
fn read_sidecar(path: &Path, max: u64) -> Option<String> {
    read_bounded_opts(path, max, false)
}

fn read_bounded_opts(path: &Path, max: u64, follow: bool) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut flags = libc::O_NONBLOCK;
    if !follow {
        flags |= libc::O_NOFOLLOW;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut text = String::new();
    file.take(max.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (u64::try_from(text.len()).ok()? <= max).then_some(text)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
