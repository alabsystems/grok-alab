// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The integration tests' rig: a fake atpkg store holding fake grok builds (bash scripts that
//! log what they were run with), a pty to run the harness on, and the harness itself.

#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The harness binary under test.
pub const HARNESS: &str = env!("CARGO_BIN_EXE_grok-alab");

/// Store id of vendor version `x.y.z` (aterm's vendor encoding).
pub const fn vendor_id(x: u64, y: u64, z: u64) -> u64 {
    1_000_000_000_000_000_000 + x * 1_000_000_000_000 + y * 1_000_000 + z
}

/// The fake grok: logs its start (build, pid, the env the harness sets, every argument) to
/// `$FAKE_GROK_LOG`, logs and exits 0 on TERM or HUP (as Grok Build 1.0.70 does, MEASURED),
/// logs INT, reads one typed line from its terminal on USR2, and then behaves as
/// `$FAKE_GROK_MODE_<build id>` says, else `$FAKE_GROK_MODE` (so one build can refuse to start
/// while another runs).
///
/// IT IDLES WITHOUT TOUCHING ITS TERMINAL. MEASURED on macOS (2026-10-24, a pty): every
/// `read(2)` of a terminal moves its access time, even one that returns no data (a
/// non-blocking read, a read a timeout interrupts), while `select`, `poll` and `kqueue` waits
/// move nothing. An idle loop of `read -t 1` on the terminal therefore reads to the harness
/// as "someone typed" every second, which made the typing test pass with nothing typed and
/// the move tests depend on when a request fell in that second. So the idle wait is bash's
/// `read -t 1` on a private FIFO (a builtin: no process runs under the fake), and the fake
/// reads its terminal only when the test says input is waiting (USR2), as a TUI reads once
/// its terminal polls readable. It ends when its harness is gone, and after 120 s anyway.
///
/// `slow-term:<n>` honours TERM `n - 1` to `n` seconds late, from the idle loop: a trap that
/// ran `sleep` itself never finished (MEASURED with macOS bash 3.2; the pending `read -t`
/// alarm cutting the trap short is the likely cause).
const FAKE: &str = r#"#!/bin/bash
log="${FAKE_GROK_LOG:?}"
mode="${FAKE_GROK_MODE_@BUILD@:-${FAKE_GROK_MODE:-idle}}"
fifo="$log.idle.$$"
mkfifo "$fifo" && exec 3<>"$fifo" && rm -f "$fifo" || exit 98
printf 'start build=@BUILD@ pid=%s autoupdater=%s harness=%s depth=%s argv=' "$$" "${GROK_DISABLE_AUTOUPDATER-}" "${GROK_ALAB_PID-}" "${GROK_ALAB_DEPTH-}" >> "$log"
for a in "$@"; do printf '[%s]' "$a" >> "$log"; done
printf '\n' >> "$log"
bg=
term_at=
on_term() { printf "term build=@BUILD@\n" >> "$log"; [ -n "$bg" ] && kill "$bg" 2>/dev/null; exit 0; }
trap on_term TERM
trap 'printf "hup build=@BUILD@\n" >> "$log"; [ -n "$bg" ] && kill "$bg" 2>/dev/null; exit 0' HUP
trap 'printf "int build=@BUILD@\n" >> "$log"' INT
trap 'IFS= read -r line; printf "read build=@BUILD@ line=%s\n" "$line" >> "$log"' USR2
case "$mode" in
  exit:*) exit "${mode#exit:}" ;;
  signal:*) trap - INT TERM HUP USR2; kill -"${mode#signal:}" $$; exit 99 ;;
  busy-child) sleep 60 & bg=$! ;;
  ignore-term) trap 'printf "term-ignored build=@BUILD@\n" >> "$log"' TERM ;;
  term-once) trap 'printf "term-ignored build=@BUILD@\n" >> "$log"; trap on_term TERM' TERM ;;
  slow-term:*) trap 'printf "term-slow build=@BUILD@\n" >> "$log"; term_at=$SECONDS' TERM ;;
esac
printf 'fake grok @BUILD@ up\r\n'
while :; do
  if [ "$mode" = busy-output ]; then printf '.'; fi
  read -t 1 -u 3 _
  if [ -n "$term_at" ] && [ $((SECONDS - term_at)) -ge "${mode#slow-term:}" ]; then on_term; fi
  kill -0 "${GROK_ALAB_PID:-$PPID}" 2>/dev/null || exit 0
  [ "$SECONDS" -ge 120 ] && exit 97
done
"#;

static SEQ: AtomicU32 = AtomicU32::new(0);

/// One test's scratch: a store prefix (with a space in it, as the real one has), a state
/// dir, a home and the fake's log.
pub struct Rig {
    pub root: PathBuf,
    pub prefix: PathBuf,
    pub state: PathBuf,
    pub home: PathBuf,
    pub log: PathBuf,
    pub path_dir: PathBuf,
}

impl Rig {
    pub fn new(name: &str) -> Rig {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "{name}-{}-{}-{nanos}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let prefix = root.join("Application Support").join("aterm").join("pkg");
        let state = root.join("state");
        let home = root.join("home");
        let path_dir = root.join("path");
        for d in [&prefix, &state, &home, &path_dir] {
            fs::create_dir_all(d).unwrap();
        }
        let log = root.join("fake-grok.log");
        fs::write(&log, "").unwrap();
        // A `grok` on PATH that must never run: the harness takes the store's build only.
        let decoy = path_dir.join("grok");
        fs::write(
            &decoy,
            format!(
                "#!/bin/sh\necho 'PATH grok ran' >> '{}'\nexit 42\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&decoy, fs::Permissions::from_mode(0o755)).unwrap();
        // Signed in, as Grok reads it (`~/.grok/auth.json`, non-empty): the harness never
        // relaunches a grok that would start signed out ([`Rig::sign_out`]).
        fs::create_dir_all(home.join(".grok")).unwrap();
        fs::write(home.join(".grok").join("auth.json"), "{}\n").unwrap();
        Rig {
            root,
            prefix,
            state,
            home,
            log,
            path_dir,
        }
    }

    /// Sign Grok out: its login file is gone, so a grok started now draws xAI's device
    /// sign-in (and the real one opens it in a browser).
    pub fn sign_out(&self) {
        fs::remove_file(self.home.join(".grok").join("auth.json")).unwrap();
    }

    pub fn program_dir(&self) -> PathBuf {
        self.prefix.join("store").join("grok")
    }

    /// Install fake build `version` (`x.y.z`) under its vendor id, ready, with its records.
    pub fn install(&self, version: (u64, u64, u64)) -> u64 {
        let id = vendor_id(version.0, version.1, version.2);
        let dir = self.program_dir().join(id.to_string());
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = FAKE.replace("@BUILD@", &id.to_string());
        let path = bin.join("grok");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let pd = self.program_dir();
        fs::write(
            pd.join(format!("{id}.ready")),
            "ok\nplatform=aarch64-macos\ncontents=bin:grok\n",
        )
        .unwrap();
        fs::write(
            pd.join(format!("{id}.vendor")),
            format!(
                "schema = 1\nprogram = \"grok\"\nversion = \"{}.{}.{}\"\nvendor = \"xAI\"\n",
                version.0, version.1, version.2
            ),
        )
        .unwrap();
        fs::write(
            pd.join(format!("{id}.shim-env")),
            "GROK_DISABLE_AUTOUPDATER=1\n",
        )
        .unwrap();
        id
    }

    /// Point `current` at build `id`, atomically (a new link renamed over the old), as atpkg
    /// flips it.
    pub fn make_current(&self, id: u64) {
        let pd = self.program_dir();
        let tmp = pd.join(format!(".current-{id}"));
        let _ = fs::remove_file(&tmp);
        std::os::unix::fs::symlink(pd.join(id.to_string()), &tmp).unwrap();
        fs::rename(&tmp, pd.join("current")).unwrap();
    }

    pub fn log_text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn journal_text(&self) -> String {
        fs::read_to_string(self.state.join("journal.log")).unwrap_or_default()
    }

    /// The fake's start lines, in order: whole lines only, since the fake writes a start
    /// line in pieces (a test that read a half-written one saw a launch with no arguments).
    pub fn starts(&self) -> Vec<Start> {
        let text = self.log_text();
        let whole = text.rsplit_once('\n').map_or("", |(done, _)| done);
        whole.lines().filter_map(Start::parse).collect()
    }

    /// The base environment: nothing inherited from the test's own.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(HARNESS);
        cmd.args(args)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", self.path_dir.display()))
            .env("HOME", &self.home)
            .env("GROK_ALAB_PKG_PREFIX", &self.prefix)
            .env("GROK_ALAB_STATE_DIR", &self.state)
            .env("GROK_ALAB_POLL_MS", "50")
            .env("FAKE_GROK_LOG", &self.log);
        cmd
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// One start line of the fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub build: u64,
    pub pid: i32,
    pub autoupdater: String,
    pub harness: String,
    pub depth: String,
    pub argv: Vec<String>,
}

impl Start {
    fn parse(line: &str) -> Option<Start> {
        let rest = line.strip_prefix("start ")?;
        let (head, argv) = rest.split_once(" argv=")?;
        let field = |name: &str| {
            head.split(' ')
                .find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_string))
        };
        let argv = argv
            .strip_prefix('[')
            .map(|a| {
                a.strip_suffix(']')
                    .unwrap_or(a)
                    .split("][")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(Start {
            build: field("build")?.parse().ok()?,
            pid: field("pid")?.parse().ok()?,
            autoupdater: field("autoupdater").unwrap_or_default(),
            harness: field("harness").unwrap_or_default(),
            depth: field("depth").unwrap_or_default(),
            argv,
        })
    }
}

/// A pty whose slave the harness runs on, with its output drained.
pub struct Pty {
    pub master: Arc<OwnedFd>,
    slave: Option<OwnedFd>,
    pub output: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    /// A new pty with echo off (typed input then moves only the input clock).
    pub fn new() -> Pty {
        let (mut m, mut s) = (0, 0);
        // SAFETY: `openpty` writes two descriptors into the live locals; the name, termios
        // and winsize arguments may be null.
        let rc = unsafe {
            libc::openpty(
                &raw mut m,
                &raw mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty");
        // SAFETY: both descriptors were just made by `openpty` and are owned by nothing else.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        // SAFETY: an all-zero termios is valid storage, filled by `tcgetattr`.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: termios calls on the slave this test owns, into the live local.
        unsafe {
            assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &raw mut t), 0);
            t.c_lflag &= !libc::ECHO;
            assert_eq!(
                libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &raw const t),
                0
            );
        }
        let master = Arc::new(master);
        let output = Arc::new(Mutex::new(Vec::new()));
        let (m2, out2) = (Arc::clone(&master), Arc::clone(&output));
        std::thread::spawn(move || {
            // SAFETY: a second handle on the master the Arc keeps open; it is never closed
            // through this `File` (forgotten below).
            let mut f = unsafe { fs::File::from_raw_fd(m2.as_raw_fd()) };
            let mut buf = [0_u8; 4096];
            loop {
                match f.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out2.lock().unwrap().extend_from_slice(&buf[..n]),
                }
            }
            std::mem::forget(f);
            drop(m2);
        });
        Pty {
            master,
            slave: Some(slave),
            output,
        }
    }

    /// Give the slave to `cmd` as its stdin, stdout and stderr.
    pub fn attach(&self, cmd: &mut Command) {
        let s = self.slave.as_ref().expect("slave");
        cmd.stdin(Stdio::from(s.try_clone().unwrap()))
            .stdout(Stdio::from(s.try_clone().unwrap()))
            .stderr(Stdio::from(s.try_clone().unwrap()));
    }

    /// Drop this test's own copy of the slave (after the harness has its own).
    pub fn release_slave(&mut self) {
        self.slave = None;
    }

    /// Type `bytes` as a person would.
    pub fn type_input(&self, bytes: &[u8]) {
        // SAFETY: writes `bytes.len()` bytes from the live slice to the master.
        let n = unsafe { libc::write(self.master.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(usize::try_from(n).ok(), Some(bytes.len()));
    }
}

/// The harness as a running process, waited with `waitpid` (stops included).
pub struct Harness {
    pub pid: libc::pid_t,
    pub status: Option<libc::c_int>,
}

impl Harness {
    #[expect(
        clippy::zombie_processes,
        reason = "reaped by `libc::waitpid` in `poll` (and in `Drop`), which also reports stops std's `Child` cannot"
    )]
    pub fn spawn(cmd: &mut Command) -> Harness {
        let child = cmd.spawn().expect("spawn the harness");
        Harness {
            pid: libc::pid_t::try_from(child.id()).unwrap(),
            status: None,
        }
    }

    /// Spawn in a new process group of its own (the harness leads it), as a shell job is.
    pub fn spawn_in_own_group(cmd: &mut Command) -> Harness {
        cmd.process_group(0);
        Harness::spawn(cmd)
    }

    pub fn signal(&self, sig: libc::c_int) {
        // SAFETY: `kill` with integers; the pid is this test's own child.
        assert_eq!(unsafe { libc::kill(self.pid, sig) }, 0, "kill {sig}");
    }

    pub fn signal_group(&self, sig: libc::c_int) {
        // SAFETY: `killpg` with integers; the group is the harness's own.
        assert_eq!(unsafe { libc::killpg(self.pid, sig) }, 0, "killpg {sig}");
    }

    /// One `waitpid` read: `Some(status)` when it ended or stopped.
    fn poll(&mut self, flags: libc::c_int) -> Option<libc::c_int> {
        if self.status.is_some() {
            return self.status;
        }
        let mut st = 0;
        // SAFETY: `waitpid` on this test's own child into the live local.
        let r = unsafe { libc::waitpid(self.pid, &raw mut st, libc::WNOHANG | flags) };
        if r == self.pid {
            if libc::WIFEXITED(st) || libc::WIFSIGNALED(st) {
                self.status = Some(st);
            }
            return Some(st);
        }
        None
    }

    /// Whether it is still running (neither ended nor reported stopped now).
    pub fn alive(&mut self) -> bool {
        self.poll(0).is_none()
    }

    /// Wait up to `limit` for it to end; kill it (and fail) past that.
    pub fn wait_end(&mut self, limit: Duration) -> libc::c_int {
        let t = Instant::now();
        while t.elapsed() < limit {
            if let Some(st) = self.status {
                return st;
            }
            if let Some(st) = self.poll(0)
                && (libc::WIFEXITED(st) || libc::WIFSIGNALED(st))
            {
                return st;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.kill_group_hard();
        panic!("the harness did not end within {limit:?}");
    }

    /// Wait up to `limit` for a stop report.
    pub fn wait_stopped(&mut self, limit: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < limit {
            if let Some(st) = self.poll(libc::WUNTRACED) {
                return libc::WIFSTOPPED(st);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn kill_group_hard(&mut self) {
        // SAFETY: plain signals to this test's own child (and its group, when it leads one).
        unsafe {
            libc::kill(self.pid, libc::SIGKILL);
        }
        let _ = self.poll(0);
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.status.is_none() {
            // A test that failed midway: leave nothing running.
            // SAFETY: plain signals to this test's own child.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            let mut st = 0;
            // SAFETY: reap it.
            unsafe {
                libc::waitpid(self.pid, &raw mut st, 0);
            }
        }
    }
}

/// Wait up to `limit` for `cond`.
pub fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// The `ps` state letters of `pid` (`T` when stopped), or empty when it is gone.
pub fn ps_state(pid: i32) -> String {
    Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Whether process `pid` exists.
pub fn exists(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the pid exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Send `sig` to process `pid` (a fake the harness started).
pub fn signal_pid(pid: i32, sig: libc::c_int) {
    // SAFETY: `kill` with integers.
    assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "kill {sig} {pid}");
}

/// Kill a fake's leftover pid (a test that failed midway).
pub fn kill_quietly(pid: i32) {
    if pid > 0 {
        // SAFETY: a plain signal to a pid this test started (through the harness).
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

pub fn os(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

pub fn path_of(p: &Path) -> String {
    p.display().to_string()
}

/// A stand-in for aterm's control socket: the rendezvous directory's `graph/<sid>` entry, the
/// socket and the token beside it, answering `harness policy`, `meta set|unset supervisor` and
/// `status` from [`AtermState`], which a test changes as it goes, and streaming
/// `subscribe @<sid> events` ([`FakeAterm::policy_event`]). It proves nothing itself: a claim
/// is admitted when [`AtermState::claim_reply`] says `OK`, and is bound to its connection
/// (released when that connection closes), as aterm binds one. [`FakeAterm::restart`] drops
/// every connection and the claim, as an aterm restart does.
pub struct FakeAterm {
    /// `XDG_RUNTIME_DIR` for the harness (holds `aterm/graph/<sid>`).
    pub xdg: PathBuf,
    /// The short directory holding the socket and its token (a socket path has ~100 bytes).
    pub sock_dir: PathBuf,
    /// The session id.
    pub sid: String,
    /// What it answers, and what it was asked.
    pub state: Arc<Mutex<AtermState>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// What a [`FakeAterm`] answers, and what it was asked.
#[derive(Debug)]
pub struct AtermState {
    /// The whole `harness policy` reply.
    pub policy: String,
    /// The reply to `meta set supervisor …` (a claim is stored only when it is `OK`).
    pub claim_reply: String,
    /// Whether a stored claim reads `supervisor_proven=1`.
    pub proven: bool,
    /// The `status` fields besides `schema=`, `supervisor=` and `supervisor_proven=`.
    pub screen: Vec<(String, String)>,
    /// The live claim's holder, and the connection it is bound to.
    pub supervisor: Option<(String, u32)>,
    /// Every request line after the handshake, `@<sid>` included (subscriptions apart).
    pub lines: Vec<String>,
    /// Every `subscribe` line.
    pub subscriptions: Vec<String>,
    /// Whether `subscribe` is refused (an aterm that gives no event stream).
    pub refuse_subscribe: bool,
    /// The events pushed so far, in order, each a whole `EVENT …` line.
    pub events: Vec<String>,
    /// How many connections authenticated.
    pub connections: u32,
    /// Every live connection, so [`FakeAterm::restart`] can drop them.
    streams: Vec<std::os::unix::net::UnixStream>,
}

/// aterm's policy reply with contract 3 served and every switch a move needs on.
pub const POLICY_V3: &str = "OK gen=1 effective=on why=enabled contract=2,3 relaunch=true \
     upgrade=true upgrade_ask=false human_grace_s=120 questions=ask";

/// WHAT ATERM REPORTS FOR A GROK SESSION TODAY (aterm main, 2026-10-25): its Grok reader has no
/// authoritative phase (`GrokReader::phase` is always a non-authoritative idle,
/// `crates/aterm-phase/src/reader.rs`), which presence turns into `agent=unknown`
/// (`crates/aterm-gui/src/presence.rs`), and `status` carries no `composer=`. Nobody typing or
/// holding the session. Every test starts from this screen, so a move on aterm's read is never
/// tested against a screen aterm does not send.
pub const GROK_SCREEN: &[(&str, &str)] = &[
    ("hold", "0"),
    ("hand", "-"),
    ("input", "clear"),
    ("agent", "unknown"),
    ("human_ms", "-"),
    ("need", "-"),
];

/// A quiet screen as an aterm WITH A MEASURED GROK READER would report it (not shipped): the
/// turn over (`agent=idle`), the composer empty (contract 3 item 9's `composer=`), nobody
/// typing or holding the session.
pub const READER_SCREEN: &[(&str, &str)] = &[
    ("hold", "0"),
    ("hand", "-"),
    ("input", "clear"),
    ("agent", "idle"),
    ("composer", "empty"),
    ("human_ms", "-"),
    ("need", "-"),
];

const FAKE_TOKEN: &str = "fake-aterm-token";

fn owned(screen: &[(&str, &str)]) -> Vec<(String, String)> {
    screen
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

impl FakeAterm {
    /// A fake aterm for session `s-<n>` under `rig`, answering `policy`, showing
    /// [`GROK_SCREEN`].
    pub fn new(rig: &Rig, policy: &str) -> FakeAterm {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let sid = format!("s-{:x}{n:x}", std::process::id());
        let xdg = rig.root.join("xdg");
        let graph = xdg.join("aterm").join("graph");
        fs::create_dir_all(&graph).unwrap();
        let sock_dir = std::env::temp_dir().join(format!("ga{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&sock_dir);
        fs::create_dir_all(&sock_dir).unwrap();
        let sock = sock_dir.join("c.sock");
        fs::write(sock_dir.join("c.sock.token"), format!("{FAKE_TOKEN}\n")).unwrap();
        fs::write(
            graph.join(&sid),
            format!("pid 1\nsock {}\n", sock.display()),
        )
        .unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(Mutex::new(AtermState {
            policy: policy.to_string(),
            claim_reply: "OK".to_string(),
            proven: true,
            screen: owned(GROK_SCREEN),
            supervisor: None,
            lines: Vec::new(),
            subscriptions: Vec::new(),
            refuse_subscribe: false,
            events: Vec::new(),
            connections: 0,
            streams: Vec::new(),
        }));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (st, sp) = (Arc::clone(&state), Arc::clone(&stop));
        let sid2 = sid.clone();
        std::thread::spawn(move || {
            let mut id = 0_u32;
            while !sp.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        id += 1;
                        if let Ok(clone) = stream.try_clone() {
                            st.lock().unwrap().streams.push(clone);
                        }
                        let (st, sp, sid) = (Arc::clone(&st), Arc::clone(&sp), sid2.clone());
                        std::thread::spawn(move || serve(stream, id, &st, &sp, &sid));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        FakeAterm {
            xdg,
            sock_dir,
            sid,
            state,
            stop,
        }
    }

    /// The environment that puts the harness in this fake's tab.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("TERM_PROGRAM".to_string(), "aterm".to_string()),
            ("ATERM_PARENT_SESSION_ID".to_string(), self.sid.clone()),
            ("XDG_RUNTIME_DIR".to_string(), path_of(&self.xdg)),
        ]
    }

    /// Set one `status` field.
    pub fn set(&self, key: &str, value: &str) {
        let mut st = self.state.lock().unwrap();
        match st.screen.iter_mut().find(|(k, _)| k == key) {
            Some(kv) => kv.1 = value.to_string(),
            None => st.screen.push((key.to_string(), value.to_string())),
        }
    }

    /// Make the `status` fields `base` with `changes` applied (`None` drops a field), in one
    /// step, so no read ever sees a screen between the two.
    pub fn screen_from(&self, base: &[(&str, &str)], changes: &[(&str, Option<&str>)]) {
        let mut st = self.state.lock().unwrap();
        st.screen = owned(base);
        for (key, value) in changes {
            st.screen.retain(|(k, _)| k != key);
            if let Some(v) = value {
                st.screen.push(((*key).to_string(), (*v).to_string()));
            }
        }
    }

    /// [`Self::screen_from`] [`READER_SCREEN`]: a screen only a measured Grok reader would give.
    pub fn screen(&self, changes: &[(&str, Option<&str>)]) {
        self.screen_from(READER_SCREEN, changes);
    }

    /// Drop one `status` field (an aterm that does not report it).
    pub fn unset(&self, key: &str) {
        self.state.lock().unwrap().screen.retain(|(k, _)| k != key);
    }

    /// Answer `harness policy` with `policy` from now on, and say so on every subscription
    /// (`EVENT <sid> harness-policy gen=<n>`), as aterm does when its policy cell moves.
    pub fn policy_event(&self, policy: &str) {
        let mut st = self.state.lock().unwrap();
        st.policy = policy.to_string();
        let generation = st.events.len() + 2;
        let line = format!("EVENT 1 harness-policy gen={generation}");
        st.events.push(line);
    }

    /// Push any other event line (one the harness must read past).
    pub fn event(&self, line: &str) {
        self.state.lock().unwrap().events.push(line.to_string());
    }

    /// An aterm restart: every connection is dropped, and the claim with them.
    pub fn restart(&self) {
        let mut st = self.state.lock().unwrap();
        st.supervisor = None;
        for s in st.streams.drain(..) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The request lines so far, without the `@<sid> ` in front.
    pub fn verbs(&self) -> Vec<String> {
        let prefix = format!("@{} ", self.sid);
        self.state
            .lock()
            .unwrap()
            .lines
            .iter()
            .map(|l| l.strip_prefix(&prefix).unwrap_or(l).to_string())
            .collect()
    }

    /// The claims asked for so far.
    pub fn claims(&self) -> Vec<String> {
        self.verbs()
            .into_iter()
            .filter(|v| v.starts_with("meta set supervisor "))
            .collect()
    }

    /// The live claim's holder.
    pub fn holder(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .supervisor
            .clone()
            .map(|(h, _)| h)
    }

    /// How many `subscribe` lines arrived.
    pub fn subscriptions(&self) -> usize {
        self.state.lock().unwrap().subscriptions.len()
    }
}

impl Drop for FakeAterm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut st = self.state.lock().unwrap();
        for s in st.streams.drain(..) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        drop(st);
        let _ = fs::remove_dir_all(&self.sock_dir);
    }
}

/// Stream the events pushed after this subscription began, until the client goes, the fake
/// stops, or a restart drops the connection.
fn stream_events(
    mut out: std::os::unix::net::UnixStream,
    state: &Arc<Mutex<AtermState>>,
    stop: &Arc<std::sync::atomic::AtomicBool>,
) {
    use std::io::Write;
    let mut sent = state.lock().unwrap().events.len();
    while !stop.load(Ordering::Relaxed) {
        let fresh: Vec<String> = {
            let st = state.lock().unwrap();
            st.events[sent.min(st.events.len())..].to_vec()
        };
        for line in &fresh {
            if out.write_all(format!("{line}\n").as_bytes()).is_err() {
                return;
            }
        }
        sent += fresh.len();
        // A dropped connection (a restart) ends the stream.
        let mut probe = [0_u8; 1];
        let _ = out.set_nonblocking(true);
        match std::io::Read::read(&mut out, &mut probe) {
            Ok(0) => return,
            Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => return,
            _ => {}
        }
        let _ = out.set_nonblocking(false);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn serve(
    stream: std::os::unix::net::UnixStream,
    id: u32,
    state: &Arc<Mutex<AtermState>>,
    stop: &Arc<std::sync::atomic::AtomicBool>,
    sid: &str,
) {
    use std::io::{BufRead, BufReader, Write};
    let _ = stream.set_nonblocking(false);
    let mut out = stream.try_clone().unwrap();
    let reader = BufReader::new(stream);
    let mut authed = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if !authed {
            if line == format!("AUTH {FAKE_TOKEN}") {
                authed = true;
                state.lock().unwrap().connections += 1;
                continue;
            }
            let _ = out.write_all(b"ERR auth\n");
            break;
        }
        if line.starts_with("subscribe ") {
            let refused = {
                let mut st = state.lock().unwrap();
                st.subscriptions.push(line.clone());
                st.refuse_subscribe || line != format!("subscribe @{sid} events")
            };
            if refused {
                let _ = out.write_all(b"ERR usage: subscribe\n");
                break;
            }
            if out
                .write_all(format!("OK subscribe 1\nsub 1 {sid}\n").as_bytes())
                .is_err()
            {
                break;
            }
            stream_events(out, state, stop);
            return;
        }
        let reply = {
            let mut st = state.lock().unwrap();
            if line == "version" {
                "OK version=fake-aterm".to_string()
            } else {
                st.lines.push(line.clone());
                let verb = line.split_once(' ').map_or("", |(_, v)| v).to_string();
                if verb == "harness policy" {
                    st.policy.clone()
                } else if let Some(rest) = verb.strip_prefix("meta set supervisor ") {
                    let reply = st.claim_reply.clone();
                    if reply == "OK" {
                        let holder = rest.split(' ').next().unwrap_or("").to_string();
                        st.supervisor = Some((holder, id));
                    }
                    reply
                } else if verb.starts_with("meta unset supervisor") {
                    st.supervisor = None;
                    "OK".to_string()
                } else if verb == "status" {
                    let mut s = "OK schema=1".to_string();
                    for (k, v) in &st.screen {
                        s.push_str(&format!(" {k}={v}"));
                    }
                    let (sup, proven) = match &st.supervisor {
                        Some((h, _)) => (h.replace('@', "%40"), u8::from(st.proven)),
                        None => ("-".to_string(), 0),
                    };
                    s.push_str(&format!(" supervisor={sup} supervisor_proven={proven}"));
                    s
                } else {
                    "ERR unknown verb".to_string()
                }
            }
        };
        if out.write_all(format!("{reply}\n").as_bytes()).is_err() {
            break;
        }
    }
    let mut st = state.lock().unwrap();
    if st.supervisor.as_ref().is_some_and(|(_, c)| *c == id) {
        st.supervisor = None;
    }
}
