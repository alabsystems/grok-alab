// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ATERM CLIENT: aterm harness contract 3, the session's claim, and aterm's read of the
//! screen.
//!
//! Under aterm the harness follows the start sequence of aterm's harness design
//! (`docs/DESIGN-harness-repos-2026-10-24.md` §3.2, §8):
//!
//! 1. Find the session's control socket: `ATERM_PARENT_SESSION_ID` names the session, the
//!    rendezvous directory's `graph/<sid>` names its socket, and the token beside the socket
//!    opens it (`AUTH <token>`, then `version`).
//! 2. Read `@<sid> harness policy`. Go on only when its `contract=` set lists
//!    [`REQUIRED_CONTRACT`] and `effective=on`. A reply without `contract=` is an aterm that
//!    predates contract 3, and the harness never claims from one (§8.2, the fail-closed rule).
//! 3. Claim: `meta set supervisor grok-alab@<pid> contract=3 scope=relaunch,upgrade
//!    version=<this harness> vendor=<grok>`. aterm proves it on the kernel's facts (this
//!    connection's peer is `<pid>`, `<pid>` leads the session's foreground group, and its image
//!    is a build of `grok-alab` from atpkg's store or its dev link) and answers `OK`, or refuses
//!    the claim whole.
//! 4. Confirm with `status`: `supervisor=grok-alab@<pid>` and `supervisor_proven=1`.
//!
//! Any step that fails is a [`Start::PassThrough`]: the harness `execve`s grok in its own place
//! ([`crate::run`]), and the tab runs grok under its own name, as if no harness existed.
//!
//! Under a held claim the safe point comes from aterm's `status` ([`Screen`],
//! [`crate::safepoint`]), not from a hand-sent signal; `SIGUSR1` stays as an override. The claim
//! is bound to this connection: it ends when the harness does, and the harness keeps the
//! connection for its whole life (re-made, and the claim re-proven, when aterm's socket goes
//! away and comes back; [`Conn::closed`] notices that on the next tick, not at the next act).
//!
//! The policy is re-read on aterm's `EVENT <sid> harness-policy` ([`Events`], a second
//! connection subscribed to the session's `events` stream), as aterm's harness design §2 asks
//! and codex-alab does, and again, fresh, before every act (a crash relaunch or a move). When it
//! goes off (Autopilot Off, `[harness] enabled = false`) the claim is given back
//! ([`Claimed::release`]) and made again once it is back on.
//!
//! This harness never types into the session: `relaunch` and `upgrade` type nothing (§8.1 item
//! 6), so it never sends `send`, `key`, `paste` or `turn`.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::env::Env;

/// The contract this harness needs before it claims a session.
pub const REQUIRED_CONTRACT: u32 = 3;

/// The holder word a claim names (`grok-alab@<pid>`).
pub const HOLDER_PREFIX: &str = "grok-alab@";

/// The duties this harness claims (aterm's roster row allows exactly these for `grok-alab`).
pub const SCOPE: &str = "relaunch,upgrade";

/// The variable aterm names the session in.
pub const SID_ENV: &str = "ATERM_PARENT_SESSION_ID";

/// The variable that leaves the harness out of a session (`--no-alab-harness` on the twin).
pub const NO_HARNESS_ENV: &str = "ATERM_NO_HARNESS";

/// How long one open or one request may take before the harness gives up on aterm.
pub const TIMEOUT: Duration = Duration::from_secs(5);

const MAX_TOKEN_BYTES: u64 = 256;
const MAX_GRAPH_BYTES: u64 = 16 * 1024;
const MAX_LINE_BYTES: u64 = 1 << 20;

/// Whether this process runs in an aterm tab (`TERM_PROGRAM=aterm` or `ATERM_CHILD=1`).
#[must_use]
pub fn under_aterm(env: &Env) -> bool {
    env.get("TERM_PROGRAM").is_some_and(|v| v == "aterm")
        || env.get("ATERM_CHILD").is_some_and(|v| v == "1")
}

/// Whether the person left the harness out (`ATERM_NO_HARNESS=1`).
#[must_use]
pub fn left_out(env: &Env) -> bool {
    env.get(NO_HARNESS_ENV).is_some_and(|v| v == "1")
}

/// aterm's rendezvous directory: `$XDG_RUNTIME_DIR/aterm`, else
/// `$HOME/Library/Application Support/aterm`.
#[must_use]
pub fn control_dir(env: &Env) -> Option<PathBuf> {
    if let Some(xdg) = env.get("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("aterm"));
    }
    let home = env.get("HOME").filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("aterm"),
    )
}

/// Whether `sid` has a session id's shape (`s-<hex>`), so it can never name a path.
#[must_use]
pub fn valid_sid(sid: &str) -> bool {
    sid.strip_prefix("s-")
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The socket a `graph/<sid>` entry names (its `sock <path>` line).
#[must_use]
pub fn graph_sock(body: &str) -> Option<PathBuf> {
    body.lines()
        .find_map(|l| l.strip_prefix("sock "))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Where the session's control socket is found again (after aterm's socket moved).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locator {
    /// The rendezvous directory.
    pub dir: PathBuf,
    /// The session id.
    pub sid: String,
}

impl Locator {
    /// The locator `env` names.
    ///
    /// # Errors
    /// Why no session can be named.
    pub fn from_env(env: &Env) -> Result<Locator, String> {
        let sid = env
            .get(SID_ENV)
            .and_then(|v| v.into_string().ok())
            .ok_or_else(|| format!("{SID_ENV} is not set"))?;
        if !valid_sid(&sid) {
            return Err(format!("{SID_ENV} is not a session id"));
        }
        let dir = control_dir(env).ok_or("neither XDG_RUNTIME_DIR nor HOME is set")?;
        Ok(Locator { dir, sid })
    }

    /// The socket the session's graph entry names now.
    ///
    /// # Errors
    /// Why there is none.
    pub fn sock(&self) -> Result<PathBuf, String> {
        let entry = self.dir.join("graph").join(&self.sid);
        let body = read_regular_file(&entry, MAX_GRAPH_BYTES)
            .map_err(|e| format!("no graph entry for {} ({e})", self.sid))?;
        graph_sock(&body).ok_or_else(|| format!("the graph entry for {} names no socket", self.sid))
    }
}

/// The token file names to try beside socket file `sock_name`, in order (aterm's own rule:
/// `aterm-<pid>.sock` has `aterm-<pid>.token`; any other socket its own token, then
/// `aterm.token`).
#[must_use]
pub fn token_names(sock_name: &str) -> Vec<String> {
    if let Some(pid) = sock_name
        .strip_suffix(".sock")
        .and_then(|n| n.strip_prefix("aterm-"))
        .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return vec![format!("aterm-{pid}.token")];
    }
    let own = if sock_name.ends_with(".sock") {
        format!("{sock_name}.token")
    } else {
        format!("aterm-sock-{sock_name}.token")
    };
    vec![own, "aterm.token".to_string()]
}

/// The token beside `sock`, read without following a link and no longer than 256 bytes.
///
/// # Errors
/// No token file, or one that is not a single token in a regular file.
pub fn read_token_beside(sock: &Path) -> io::Result<String> {
    let (Some(dir), Some(name)) = (sock.parent(), sock.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the socket path names no directory",
        ));
    };
    let name = std::fs::read_link(sock)
        .ok()
        .and_then(|t| t.file_name().map(std::ffi::OsStr::to_os_string))
        .unwrap_or_else(|| name.to_os_string());
    let mut miss = None;
    for candidate in token_names(&name.to_string_lossy()) {
        match read_token_file(&dir.join(candidate)) {
            Ok(token) => return Ok(token),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(e),
            Err(e) => {
                miss.get_or_insert(e);
            }
        }
    }
    Err(miss
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no token beside the socket")))
}

fn read_regular_file(path: &Path, max: u64) -> io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                bad("the path is a link")
            } else {
                e
            }
        })?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > max {
        return Err(bad("the path is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    // One lookahead byte detects growth past the bound. At u64::MAX no larger
    // byte count is representable, so that bound needs no lookahead byte.
    let read_limit = max.checked_add(1).unwrap_or(max);
    file.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(bad("the file exceeds its bound"));
    }
    String::from_utf8(bytes).map_err(|_| bad("the file is not UTF-8"))
}

fn read_token_file(path: &Path) -> io::Result<String> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    let raw = read_regular_file(path, MAX_TOKEN_BYTES)?;
    let token = raw.trim();
    if token.is_empty() || token.contains(['\r', '\n', ' ', '\t']) {
        return Err(bad("the token file is not one token"));
    }
    Ok(token.to_string())
}

/// One authenticated connection to the session's control socket.
#[derive(Debug)]
pub struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    sid: String,
    /// What aterm's `version` answered (after `OK `).
    pub version: String,
}

impl Conn {
    /// Connect to `sock`, authenticate with the token beside it and read aterm's version.
    ///
    /// # Errors
    /// The socket, the token or the handshake failed.
    pub fn open(sock: &Path, sid: &str, timeout: Duration) -> io::Result<Conn> {
        let stream = UnixStream::connect(sock)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let token = read_token_beside(sock)?;
        let writer = stream.try_clone()?;
        let mut conn = Conn {
            reader: BufReader::new(stream),
            writer,
            sid: sid.to_string(),
            version: String::new(),
        };
        conn.writer
            .write_all(format!("AUTH {token}\nversion\n").as_bytes())?;
        conn.writer.flush()?;
        let line = conn.line()?;
        match line.strip_prefix("OK ") {
            Some(version) => {
                conn.version = version.to_string();
                Ok(conn)
            }
            None => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("aterm refused the connection: {line}"),
            )),
        }
    }

    fn line(&mut self) -> io::Result<String> {
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 || buf.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "aterm closed the connection",
            ));
        }
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Send `@<sid> <verb>` and read aterm's one-line reply.
    ///
    /// # Errors
    /// The connection failed (an `ERR` reply is a reply, not an error).
    pub fn ask(&mut self, verb: &str) -> io::Result<String> {
        if verb.contains(['\n', '\r']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a verb is one line",
            ));
        }
        self.writer
            .write_all(format!("@{} {verb}\n", self.sid).as_bytes())?;
        self.writer.flush()?;
        self.line()
    }

    /// Whether aterm closed this connection, read without waiting and without sending a word.
    /// aterm sends nothing unasked on a request connection, so a connection that polls readable
    /// between requests has ended (aterm restarted, or a seamless update dropped it), and the
    /// claim bound to it with it.
    #[must_use]
    pub fn closed(&self) -> bool {
        use std::os::fd::AsRawFd;
        if !self.reader.buffer().is_empty() {
            return false;
        }
        let fd = self.writer.as_raw_fd();
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll` on one live pollfd this function owns, with no wait.
        let n = unsafe { libc::poll(&raw mut p, 1, 0) };
        if n <= 0 {
            return false;
        }
        if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return true;
        }
        let mut byte = 0_u8;
        // SAFETY: a one-byte peek into the live local on this connection's socket.
        let got = unsafe {
            libc::recv(
                fd,
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        got == 0
    }
}

/// The `subscribe` verb that streams the session's lifecycle events (`EVENT <sid> …`).
#[must_use]
pub fn subscribe_verb(sid: &str) -> String {
    format!("subscribe @{sid} events")
}

/// What one [`Events::drain`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drained {
    /// An `EVENT <sid> harness-policy gen=<n>` arrived: the policy moved.
    pub policy_moved: bool,
}

/// A second connection, subscribed to the session's `events` stream, read for
/// `EVENT <sid> harness-policy` (aterm's harness design §2: a harness re-reads its policy on
/// that event, never on a clock). Every other event is read and dropped.
#[derive(Debug)]
pub struct Events {
    stream: UnixStream,
    pending: Vec<u8>,
}

impl Events {
    /// Connect to `sock`, authenticate, and subscribe to session `sid`'s events.
    ///
    /// # Errors
    /// The socket, the token, the handshake or the subscription failed (an aterm that refuses
    /// `subscribe` answers `ERR`).
    pub fn open(sock: &Path, sid: &str, timeout: Duration) -> io::Result<Events> {
        let mut conn = Conn::open(sock, sid, timeout)?;
        conn.writer
            .write_all(format!("{}\n", subscribe_verb(sid)).as_bytes())?;
        conn.writer.flush()?;
        let ack = conn.line()?;
        let Some(n) = ack
            .strip_prefix("OK subscribe ")
            .and_then(|n| n.trim().parse::<usize>().ok())
        else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("aterm refused the subscription: {ack}"),
            ));
        };
        for _ in 0..n {
            let row = conn.line()?;
            if !row.starts_with("sub ") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the subscription's ack named no target: {row}"),
                ));
            }
        }
        let Conn { reader, writer, .. } = conn;
        // Whatever the handshake read past the ack is the stream's first bytes.
        let pending = reader.buffer().to_vec();
        drop(reader);
        writer.set_nonblocking(true)?;
        Ok(Events {
            stream: writer,
            pending,
        })
    }

    /// Read every event that has arrived, without waiting.
    ///
    /// # Errors
    /// aterm closed the stream (it restarted, or dropped the subscription).
    pub fn drain(&mut self) -> io::Result<Drained> {
        let mut buf = [0_u8; 4096];
        let mut closed = false;
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(n) => {
                    self.pending.extend_from_slice(&buf[..n]);
                    if self.pending.len() as u64 > MAX_LINE_BYTES {
                        // A line past the bound is no event of ours; keep the stream readable.
                        self.pending.clear();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let mut out = Drained::default();
        // Every complete line, up to the last newline; the partial line after it waits for the
        // next read. Taken by `get`, so no range here can be out of bounds.
        let complete = self
            .pending
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |last| last.saturating_add(1));
        for line in self
            .pending
            .get(..complete)
            .unwrap_or_default()
            .split(|b| *b == b'\n')
        {
            let line = String::from_utf8_lossy(line);
            let mut words = line.split_ascii_whitespace();
            if words.next() == Some("EVENT") && words.nth(1) == Some("harness-policy") {
                out.policy_moved = true;
            }
        }
        self.pending = self
            .pending
            .get(complete..)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if closed {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "aterm closed the event stream",
            ));
        }
        Ok(out)
    }
}

/// A reply's `key=value` fields (after its `OK`), the values as sent (still pct-encoded).
#[must_use]
pub fn fields(reply: &str) -> BTreeMap<String, String> {
    reply
        .split_ascii_whitespace()
        .skip_while(|w| *w == "OK")
        .filter_map(|w| w.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// `%XX`-decode a wire value.
#[must_use]
pub fn pct_decode(s: &str) -> String {
    let hex = |b: u8| {
        char::from(b)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn switch(f: &BTreeMap<String, String>, key: &str) -> Option<bool> {
    match f.get(key).map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

/// What `harness policy` answered, as far as this harness reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The contracts aterm serves (`contract=2,3`); empty when the reply carries none.
    pub contracts: Vec<u32>,
    /// `effective=on`.
    pub effective: bool,
    /// `why=`.
    pub why: String,
    /// `[harness] upgrade`.
    pub upgrade: Option<bool>,
    /// `[harness] upgrade_ask`.
    pub upgrade_ask: Option<bool>,
    /// `[harness] relaunch`.
    pub relaunch: Option<bool>,
    /// `[harness] human_grace_s`.
    pub human_grace: Option<Duration>,
}

impl Policy {
    /// Read a `harness policy` reply.
    ///
    /// # Errors
    /// The reply is not `OK` (aterm refused or does not know the verb).
    pub fn parse(reply: &str) -> Result<Policy, String> {
        if reply != "OK" && !reply.starts_with("OK ") {
            return Err(format!("harness policy answered {reply}"));
        }
        let f = fields(reply);
        // A contract word that is not a number is no contract this harness can serve; the
        // rest of the set stands.
        let contracts = f
            .get("contract")
            .map(|v| v.split(',').filter_map(|n| n.parse::<u32>().ok()).collect())
            .unwrap_or_default();
        Ok(Policy {
            contracts,
            effective: f.get("effective").is_some_and(|v| v == "on"),
            why: f
                .get("why")
                .map_or_else(|| "-".to_string(), |v| pct_decode(v)),
            upgrade: switch(&f, "upgrade"),
            upgrade_ask: switch(&f, "upgrade_ask"),
            relaunch: switch(&f, "relaunch"),
            human_grace: f
                .get("human_grace_s")
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs),
        })
    }

    /// Whether aterm serves contract `n`.
    #[must_use]
    pub fn serves(&self, n: u32) -> bool {
        self.contracts.contains(&n)
    }
}

/// The vendor version as a claim's `vendor=` word, or `None` when it is not one (aterm takes
/// at most 64 bytes of ASCII letters, digits and `.-+_`).
#[must_use]
pub fn vendor_word(version: Option<&str>) -> Option<&str> {
    version.filter(|v| {
        !v.is_empty()
            && v.len() <= 64
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    })
}

/// This harness's holder word for process `pid`.
#[must_use]
pub fn holder(pid: u32) -> String {
    format!("{HOLDER_PREFIX}{pid}")
}

/// The claim's verb: `meta set supervisor grok-alab@<pid> contract=3 scope=relaunch,upgrade
/// version=<version> [vendor=<vendor>]`.
#[must_use]
pub fn claim_verb(pid: u32, version: &str, vendor: Option<&str>) -> String {
    let mut verb = format!(
        "meta set supervisor {} contract={REQUIRED_CONTRACT} scope={SCOPE} version={version}",
        holder(pid)
    );
    if let Some(v) = vendor_word(vendor) {
        verb.push_str(" vendor=");
        verb.push_str(v);
    }
    verb
}

/// When a person last typed, as `status human_ms=` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Human {
    /// `human_ms=-`: nobody has typed in this session.
    Never,
    /// `human_ms=<n>`.
    Ago(Duration),
    /// No reading.
    Unknown,
}

/// What `status` says of the session, as far as a safe point reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// `supervisor=` (decoded), `None` for `-` or absent.
    pub supervisor: Option<String>,
    /// `supervisor_proven=1`.
    pub proven: bool,
    /// `agent=`.
    pub agent: Option<String>,
    /// `composer=` (contract 3 §8.1 item 9); `None` when aterm does not report it.
    pub composer: Option<String>,
    /// `human_ms=`.
    pub human: Human,
    /// `hand=`.
    pub hand: Option<String>,
    /// `hold=`.
    pub hold: Option<String>,
    /// `input=`.
    pub input: Option<String>,
    /// `need=`.
    pub need: Option<String>,
}

impl Screen {
    /// Read a `status` reply.
    ///
    /// # Errors
    /// The reply is not `OK`, or is not schema 1.
    pub fn parse(reply: &str) -> Result<Screen, String> {
        if !reply.starts_with("OK ") {
            return Err(format!("status answered {reply}"));
        }
        let f = fields(reply);
        let schema = f
            .get("schema")
            .and_then(|s| s.split('.').next())
            .and_then(|s| s.parse::<u32>().ok());
        if schema != Some(1) {
            return Err("status is not schema 1".to_string());
        }
        let word = |k: &str| f.get(k).map(|v| pct_decode(v));
        Ok(Screen {
            supervisor: word("supervisor").filter(|s| s != "-"),
            proven: f.get("supervisor_proven").is_some_and(|v| v == "1"),
            agent: word("agent"),
            composer: word("composer"),
            human: match f.get("human_ms").map(String::as_str) {
                Some("-") => Human::Never,
                Some(ms) => ms
                    .parse::<u64>()
                    .map_or(Human::Unknown, |ms| Human::Ago(Duration::from_millis(ms))),
                None => Human::Unknown,
            },
            hand: word("hand"),
            hold: word("hold"),
            input: word("input"),
            need: word("need"),
        })
    }

    /// Whether `holder`'s claim is the session's, proven.
    #[must_use]
    pub fn held_by(&self, holder: &str) -> bool {
        self.proven && self.supervisor.as_deref() == Some(holder)
    }
}

/// A claim aterm proved, and the connection it is bound to.
#[derive(Debug)]
pub struct Claimed {
    /// The connection the claim is bound to (kept open for the harness's whole life).
    pub conn: Conn,
    /// `grok-alab@<pid>`.
    pub holder: String,
    /// The policy as last read.
    pub policy: Policy,
    /// Where the socket is found again.
    pub locator: Locator,
    /// The session's event stream, read for `harness-policy` ([`Events`]); `None` when aterm
    /// refused the subscription or it ended (the policy is then re-read on a clock, and the
    /// subscription made again).
    pub events: Option<Events>,
}

impl Claimed {
    /// Give the claim back (`meta unset supervisor holder=<holder>`), keeping the connection
    /// and the subscription, so it can be made again on them.
    ///
    /// # Errors
    /// The connection failed, or aterm refused.
    pub fn release(&mut self) -> Result<(), String> {
        let reply = self
            .conn
            .ask(&format!("meta unset supervisor holder={}", self.holder))
            .map_err(|e| e.to_string())?;
        if reply == "OK" || reply.starts_with("OK ") {
            Ok(())
        } else {
            Err(reply)
        }
    }

    /// Claim again on this connection and confirm it with `status`; a claim aterm stored but
    /// did not confirm as proven is let go.
    ///
    /// # Errors
    /// The connection failed, aterm refused, or the claim did not read back proven.
    pub fn claim_again(
        &mut self,
        pid: u32,
        version: &str,
        vendor: Option<&str>,
    ) -> Result<(), String> {
        self.reclaim(pid, version, vendor)
            .map_err(|why| format!("aterm refused the claim: {why}"))?;
        let screen = self.screen()?;
        if screen.held_by(&self.holder) {
            return Ok(());
        }
        let _ = self.release();
        Err(format!(
            "aterm did not confirm the claim (supervisor={} supervisor_proven={})",
            screen.supervisor.as_deref().unwrap_or("-"),
            u8::from(screen.proven)
        ))
    }

    /// Re-read `harness policy` into [`Self::policy`].
    ///
    /// # Errors
    /// The connection failed, or the reply is not a policy.
    pub fn refresh_policy(&mut self) -> Result<&Policy, String> {
        let reply = self.conn.ask("harness policy").map_err(|e| e.to_string())?;
        self.policy = Policy::parse(&reply)?;
        Ok(&self.policy)
    }

    /// Read `status`.
    ///
    /// # Errors
    /// The connection failed, or the reply is not a status.
    pub fn screen(&mut self) -> Result<Screen, String> {
        let reply = self.conn.ask("status").map_err(|e| e.to_string())?;
        Screen::parse(&reply)
    }

    /// Claim again on this connection (a new `vendor=` after a move).
    ///
    /// # Errors
    /// The connection failed, or aterm refused.
    pub fn reclaim(&mut self, pid: u32, version: &str, vendor: Option<&str>) -> Result<(), String> {
        let reply = self
            .conn
            .ask(&claim_verb(pid, version, vendor))
            .map_err(|e| e.to_string())?;
        if reply == "OK" || reply.starts_with("OK ") {
            Ok(())
        } else {
            Err(reply)
        }
    }
}

/// How the start sequence ended.
#[derive(Debug)]
pub enum Start {
    /// A proven claim: supervise.
    Claimed(Box<Claimed>),
    /// Get out of the way: exec grok in place, for this reason.
    PassThrough(String),
}

/// Run the start sequence's aterm steps (socket, policy, claim, confirmation) for process
/// `pid`, harness `version` and vendor version `vendor`.
#[must_use]
pub fn start(
    locator: &Locator,
    pid: u32,
    version: &str,
    vendor: Option<&str>,
    timeout: Duration,
) -> Start {
    match claim(locator, pid, version, vendor, timeout) {
        Ok(c) => Start::Claimed(Box::new(c)),
        Err(why) => Start::PassThrough(why),
    }
}

/// [`start`]'s steps, as a result.
///
/// # Errors
/// Why the harness does not hold the session.
pub fn claim(
    locator: &Locator,
    pid: u32,
    version: &str,
    vendor: Option<&str>,
    timeout: Duration,
) -> Result<Claimed, String> {
    let sock = locator.sock()?;
    let mut conn = Conn::open(&sock, &locator.sid, timeout)
        .map_err(|e| format!("cannot open aterm's control socket {}: {e}", sock.display()))?;
    let reply = conn
        .ask("harness policy")
        .map_err(|e| format!("harness policy: {e}"))?;
    let policy = Policy::parse(&reply)?;
    if !policy.serves(REQUIRED_CONTRACT) {
        return Err(format!(
            "aterm does not serve harness contract {REQUIRED_CONTRACT} (contract={})",
            if policy.contracts.is_empty() {
                "absent".to_string()
            } else {
                policy
                    .contracts
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            }
        ));
    }
    if !policy.effective {
        return Err(format!(
            "aterm's harness policy is off for this session (why={})",
            policy.why
        ));
    }
    let mut claimed = Claimed {
        conn,
        holder: holder(pid),
        policy,
        locator: locator.clone(),
        events: None,
    };
    // Never leave a claim aterm did not confirm as proven standing ([`Claimed::claim_again`]
    // lets it go).
    claimed.claim_again(pid, version, vendor)?;
    // The policy is re-read on its event from here on; an aterm that refuses the subscription
    // leaves the harness re-reading it on a clock.
    claimed.events = Events::open(&sock, &locator.sid, timeout).ok();
    Ok(claimed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    #[test]
    fn regular_file_bounds_include_zero_and_u64_max() {
        let scratch = crate::scratch::Scratch::new("regular-file-bounds");
        let path = scratch.join("input");
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_regular_file(&path, 0).unwrap(), "");

        std::fs::write(&path, "ok").unwrap();
        assert_eq!(read_regular_file(&path, 2).unwrap(), "ok");
        assert_eq!(
            read_regular_file(&path, 1).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(read_regular_file(&path, u64::MAX).unwrap(), "ok");

        std::fs::write(&path, [0xff]).unwrap();
        assert_eq!(
            read_regular_file(&path, u64::MAX).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn aterm_is_named_by_its_variables_and_the_session_by_its_id() {
        assert!(under_aterm(&env(&[("TERM_PROGRAM", "aterm")])));
        assert!(under_aterm(&env(&[("ATERM_CHILD", "1")])));
        assert!(!under_aterm(&env(&[("TERM_PROGRAM", "Apple_Terminal")])));
        assert!(left_out(&env(&[(NO_HARNESS_ENV, "1")])));
        assert!(!left_out(&env(&[(NO_HARNESS_ENV, "0")])));
        assert!(valid_sid("s-00ff"));
        for bad in ["s-", "s-xyz", "../x", "s-1/2", "1f"] {
            assert!(!valid_sid(bad), "{bad}");
        }
        let l =
            Locator::from_env(&env(&[(SID_ENV, "s-1a"), ("XDG_RUNTIME_DIR", "/run/u")])).unwrap();
        assert_eq!(l.dir, PathBuf::from("/run/u/aterm"));
        let l = Locator::from_env(&env(&[(SID_ENV, "s-1a"), ("HOME", "/h")])).unwrap();
        assert_eq!(l.dir, PathBuf::from("/h/Library/Application Support/aterm"));
        assert!(Locator::from_env(&env(&[(SID_ENV, "s-../x"), ("HOME", "/h")])).is_err());
        assert!(Locator::from_env(&env(&[("HOME", "/h")])).is_err());
        assert_eq!(
            graph_sock("pid 1\nsock /a/b.sock\n"),
            Some(PathBuf::from("/a/b.sock"))
        );
        assert_eq!(graph_sock("sock relative.sock\n"), None);
        assert_eq!(graph_sock("sock \n"), None);

        let scratch = crate::scratch::Scratch::new("aterm-graph");
        let graph = scratch.join("graph");
        std::fs::create_dir_all(&graph).unwrap();
        let real = scratch.join("real");
        std::fs::write(&real, "sock /a/b.sock\n").unwrap();
        std::os::unix::fs::symlink(&real, graph.join("s-1a")).unwrap();
        let loc = Locator {
            dir: scratch.to_path_buf(),
            sid: "s-1a".to_string(),
        };
        assert!(loc.sock().is_err(), "a symlinked graph entry is refused");
        std::fs::remove_file(graph.join("s-1a")).unwrap();
        std::fs::write(graph.join("s-1a"), "sock /a/b.sock\n").unwrap();
        assert_eq!(loc.sock().unwrap(), PathBuf::from("/a/b.sock"));
    }

    #[test]
    fn the_token_is_found_by_aterms_rule() {
        assert_eq!(token_names("aterm-77.sock"), ["aterm-77.token"]);
        assert_eq!(token_names("c.sock"), ["c.sock.token", "aterm.token"]);
        assert_eq!(token_names("ctl"), ["aterm-sock-ctl.token", "aterm.token"]);
    }

    #[test]
    fn the_policy_reply_names_the_served_contracts() {
        let p = Policy::parse(
            "OK gen=4 effective=on why=enabled contract=2,3 relaunch=true upgrade=true \
             upgrade_ask=false human_grace_s=90 questions=ask",
        )
        .unwrap();
        assert!(p.serves(3) && p.serves(2) && !p.serves(4));
        assert!(p.effective);
        assert_eq!(
            (p.upgrade, p.upgrade_ask, p.relaunch),
            (Some(true), Some(false), Some(true))
        );
        assert_eq!(p.human_grace, Some(Duration::from_secs(90)));
        // An aterm that predates contract 3 says no `contract=`: nothing is served.
        let old = Policy::parse("OK gen=4 effective=on why=enabled upgrade=true").unwrap();
        assert!(old.contracts.is_empty() && !old.serves(3));
        let two = Policy::parse("OK gen=1 effective=on why=enabled contract=2").unwrap();
        assert!(!two.serves(3));
        let junk = Policy::parse("OK gen=1 effective=on contract=x,3").unwrap();
        assert!(junk.serves(3));
        let off = Policy::parse("OK gen=1 effective=off why=autopilot-off contract=2,3").unwrap();
        assert!(!off.effective);
        assert_eq!(off.why, "autopilot-off");
        assert!(Policy::parse("ERR usage: harness policy").is_err());
        assert!(Policy::parse("OKAY contract=3").is_err());
    }

    #[test]
    fn the_claim_names_its_contract_scope_and_versions() {
        assert_eq!(
            claim_verb(412, "0.2.0", Some("1.0.70")),
            "meta set supervisor grok-alab@412 contract=3 scope=relaunch,upgrade version=0.2.0 vendor=1.0.70"
        );
        // A vendor version that is no wire word is left out, never sent half-escaped.
        assert_eq!(
            claim_verb(412, "0.2.0", Some("build 7")),
            "meta set supervisor grok-alab@412 contract=3 scope=relaunch,upgrade version=0.2.0"
        );
        assert_eq!(vendor_word(Some(&"9".repeat(65))), None);
        assert_eq!(vendor_word(Some("1.0.70+g1")), Some("1.0.70+g1"));
        assert_eq!(vendor_word(None), None);
    }

    #[test]
    fn the_status_reply_reads_as_a_screen() {
        let s = Screen::parse(
            "OK schema=1.4 hold=0 hand=- input=clear agent=idle composer=empty human_ms=- \
             need=- supervisor=grok-alab%40412 supervisor_proven=1",
        )
        .unwrap();
        assert!(s.held_by("grok-alab@412"));
        assert!(!s.held_by("grok-alab@413"));
        assert_eq!(s.human, Human::Never);
        assert_eq!(s.composer.as_deref(), Some("empty"));
        let s =
            Screen::parse("OK schema=1 supervisor=grok-alab@1 supervisor_proven=0 human_ms=250")
                .unwrap();
        assert!(!s.held_by("grok-alab@1"), "an unproven claim is not held");
        assert_eq!(s.human, Human::Ago(Duration::from_millis(250)));
        assert_eq!(s.composer, None);
        assert_eq!(
            Screen::parse("OK schema=1 human_ms=x").unwrap().human,
            Human::Unknown
        );
        assert!(Screen::parse("OK schema=2 hold=0").is_err());
        assert!(Screen::parse("ERR no such session").is_err());
        assert_eq!(pct_decode("a%20b%zz%4"), "a b%zz%4");
    }
}
