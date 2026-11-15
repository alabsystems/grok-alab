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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
/// EVERY TRAP AND EVERY MODE'S STATE IS SET BEFORE THE START LINE: a test that has read a start
/// line may signal that fake at once (a TERM that came before its trap would kill it rather
/// than end it, and a busy child not yet started would let a move through). Only the modes that
/// end it (`exit:<n>`, `signal:<sig>`) act after the start line, so their start is still logged.
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
/// `held-term` logs a TERM and honours it only once the test says so ([`Rig::release_term`]),
/// from the idle loop, so a late answer to the harness's SIGTERM comes when the test has seen
/// what it must see first, never on a clock a loaded machine can outrun. (A trap that waited
/// itself never finished: MEASURED with macOS bash 3.2; the pending `read -t` alarm cutting the
/// trap short is the likely cause.)
const FAKE: &str = r#"#!/bin/bash
log="${FAKE_GROK_LOG:?}"
mode="${FAKE_GROK_MODE_@BUILD@:-${FAKE_GROK_MODE:-idle}}"
fifo="$log.idle.$$"
mkfifo "$fifo" && exec 3<>"$fifo" && rm -f "$fifo" || exit 98
bg=
held=
on_term() { printf "term build=@BUILD@\n" >> "$log"; [ -n "$bg" ] && kill "$bg" 2>/dev/null; exit 0; }
trap on_term TERM
trap 'printf "hup build=@BUILD@\n" >> "$log"; [ -n "$bg" ] && kill "$bg" 2>/dev/null; exit 0' HUP
trap 'printf "int build=@BUILD@\n" >> "$log"' INT
trap 'IFS= read -r line; printf "read build=@BUILD@ line=%s\n" "$line" >> "$log"' USR2
case "$mode" in
  busy-child) sleep 60 & bg=$! ;;
  ignore-term) trap 'printf "term-ignored build=@BUILD@\n" >> "$log"' TERM ;;
  term-once) trap 'printf "term-ignored build=@BUILD@\n" >> "$log"; trap on_term TERM' TERM ;;
  held-term) trap 'printf "term-held build=@BUILD@\n" >> "$log"; held=1' TERM ;;
esac
printf 'start build=@BUILD@ pid=%s autoupdater=%s harness=%s depth=%s argv=' "$$" "${GROK_DISABLE_AUTOUPDATER-}" "${GROK_ALAB_PID-}" "${GROK_ALAB_DEPTH-}" >> "$log"
for a in "$@"; do printf '[%s]' "$a" >> "$log"; done
printf '\n' >> "$log"
case "$mode" in
  exit:*) exit "${mode#exit:}" ;;
  signal:*) trap - INT TERM HUP USR2; kill -"${mode#signal:}" $$; exit 99 ;;
esac
printf 'fake grok @BUILD@ up\r\n'
while :; do
  if [ "$mode" = busy-output ]; then printf '.'; fi
  read -t 1 -u 3 _
  if [ -n "$held" ] && [ -e "$log.release" ]; then on_term; fi
  kill -0 "${GROK_ALAB_PID:-$PPID}" 2>/dev/null || exit 0
  [ "$SECONDS" -ge 120 ] && exit 97
done
"#;

static SEQ: AtomicU32 = AtomicU32::new(0);

/// The harness's poll as [`Rig::command`] sets it (`GROK_ALAB_POLL_MS`): one tick of its loop,
/// on which it reads the store, its signals, its child and (under a claim) aterm.
pub const POLL: Duration = Duration::from_millis(50);

/// `n` ticks of the harness's loop: the window a test watches for something that must NOT
/// happen. A window proves an absence only for the ticks the harness ran in it, so it is
/// counted in the harness's own ticks, and is no stand-in for a state that can be waited on.
pub fn ticks(n: u32) -> Duration {
    POLL * n
}

/// The deadline of every wait for a condition: a passing wait returns as soon as its condition
/// holds, so the ceiling costs nothing, and a loaded machine (a build beside the suite) can
/// take seconds over a step that takes milliseconds alone.
pub const END: Duration = Duration::from_secs(30);

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

    /// Let a `held-term` fake honour the TERM it holds (on its next idle tick).
    pub fn release_term(&self) {
        fs::write(format!("{}.release", self.log.display()), "").unwrap();
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
            .env("GROK_ALAB_POLL_MS", POLL.as_millis().to_string())
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
    /// The slave's device path, whose times the harness reads its quiet from.
    slave_path: PathBuf,
    pub output: Arc<Mutex<Vec<u8>>>,
    /// Set once the master reads end of file: every slave is closed and its output drained.
    drained: Arc<AtomicBool>,
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
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: `ttyname_r` writes at most `name.len()` bytes into the live local buffer.
        let rc = unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) };
        assert_eq!(rc, 0, "ttyname_r");
        // SAFETY: on success `ttyname_r` wrote a NUL-terminated name into `name`.
        let slave_path = PathBuf::from(
            unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        );
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
        let drained = Arc::new(AtomicBool::new(false));
        let (m2, out2, done) = (
            Arc::clone(&master),
            Arc::clone(&output),
            Arc::clone(&drained),
        );
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
            done.store(true, Ordering::SeqCst);
        });
        Pty {
            master,
            slave: Some(slave),
            slave_path,
            output,
            drained,
        }
    }

    /// Everything the programs on the slave wrote so far.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// How long ago a program on the slave last read typed input and last wrote output, read
    /// as the harness reads them (the slave's access and modification times,
    /// `grok_alab::procs::tty_quiet`; a `stat` of its path reads the times an `fstat` of the
    /// harness's stdin reads). macOS stamps them lazily, at the next `stat`, so an event only
    /// ever reads as newer than it was, never older.
    pub fn quiet(&self) -> (Duration, Duration) {
        use std::os::unix::fs::MetadataExt as _;
        let st = fs::metadata(&self.slave_path).unwrap();
        let at = |secs: i64, nanos: i64| {
            UNIX_EPOCH + Duration::new(u64::try_from(secs).unwrap(), u32::try_from(nanos).unwrap())
        };
        let now = SystemTime::now();
        let age = |t: SystemTime| now.duration_since(t).unwrap_or(Duration::ZERO);
        (
            age(at(st.atime(), st.atime_nsec())),
            age(at(st.mtime(), st.mtime_nsec())),
        )
    }

    /// Wait until the terminal has carried no input for `input` and no output for `output`, as
    /// the harness will read it at its next request: the state a move's quiet gates ask for.
    /// The ages only grow while nothing moves the terminal, so a request sent after this
    /// returns true finds both gates passed.
    pub fn wait_quiet(&self, input: Duration, output: Duration) -> bool {
        wait_until(END, || {
            let (i, o) = self.quiet();
            i >= input && o >= output
        })
    }

    /// Wait until every program on the slave has closed it and the master has read all they
    /// wrote (the harness and its children are gone).
    pub fn wait_drained(&self) -> bool {
        wait_until(END, || self.drained.load(Ordering::SeqCst))
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

/// Wait until the fake with pid `pid` has said `up` on `pty` (every trap set, its first output
/// written), or has ended without saying it (a mode that exits at once). Its answer is not
/// needed: a fake that ended is the test's own business. (The terminal's output processing
/// turns the fake's `\r\n` into `\r\r\n`, so the line is matched up to its first `\r`.)
pub fn wait_up(pty: &Pty, pid: i32) {
    let _ = wait_until(END, || {
        pty.text().contains(" up\r") || {
            let state = ps_state(pid);
            state.is_empty() || state.contains('Z')
        }
    });
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
