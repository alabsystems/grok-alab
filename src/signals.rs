// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S SIGNALS: a self-pipe the handlers write to, so the one supervising loop
//! reads every signal in order with nothing done inside a handler but a `write(2)`.
//!
//! The vendor runs in the harness's own process group on the inherited terminal, so the
//! terminal's INT, QUIT and TSTP reach both. The policy (aterm's wrapper shape,
//! `synthesis.md` B2):
//! - INT and QUIT belong to the child: the harness catches them and does nothing.
//! - TSTP is followed: the harness stops itself only once its child has stopped.
//! - HUP and TERM are passed on to the child; the session then ends with the child.
//! - CONT is passed on, USR1 asks for a safe point, CHLD wakes the loop.
//!
//! A signal that was IGNORED when the harness started is left ignored, so the child inherits
//! exactly the disposition it would have had run directly. A caught signal reverts to its
//! default in the child at `exec` by itself.
//!
//! Handlers are installed with `signal(3)` (BSD semantics: restarting, nothing else masked),
//! which reads a disposition only by replacing it: the handler goes in first, a signal found
//! ignored and kept so is set back at once, and [`Signals::drain`] never reports one that
//! arrived in between, so it stays ignored as far as the harness acts on it.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

/// The write end of the self-pipe, for the handler.
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// The signals the harness catches (when not inherited as ignored).
pub const CAUGHT: [libc::c_int; 8] = [
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGHUP,
    libc::SIGTERM,
    libc::SIGTSTP,
    libc::SIGCONT,
    libc::SIGCHLD,
    libc::SIGUSR1,
];

extern "C" fn on_signal(sig: libc::c_int) {
    let saved = errno::errno();
    let fd = WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = u8::try_from(sig).unwrap_or(0);
        // SAFETY: `write(2)` is async-signal-safe; the buffer is one live local byte. A full
        // pipe (EAGAIN) drops the byte: the loop still wakes on the bytes already queued.
        unsafe {
            libc::write(fd, (&raw const byte).cast(), 1);
        }
    }
    errno::set_errno(saved);
}

/// Whether `sig`, found ignored at start, stays ignored (and is not caught).
fn kept_ignored(sig: libc::c_int) -> bool {
    matches!(
        sig,
        libc::SIGINT | libc::SIGQUIT | libc::SIGHUP | libc::SIGTERM | libc::SIGTSTP
    )
}

/// The installed handlers and the pipe they write to.
#[derive(Debug)]
pub struct Signals {
    read: OwnedFd,
    _write: OwnedFd,
    /// The signals that were ignored at start and stay ignored.
    pub inherited_ignored: Vec<libc::c_int>,
}

impl Signals {
    /// Make the pipe and install the handlers.
    ///
    /// # Errors
    /// The pipe or a `sigaction` failed.
    pub fn install() -> io::Result<Signals> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `pipe` writes two descriptors into the live two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just made by `pipe` and are owned by nothing else.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        for fd in [read.as_raw_fd(), write.as_raw_fd()] {
            set_flag(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK)?;
            set_flag(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)?;
        }
        WRITE_FD.store(write.as_raw_fd(), Ordering::Relaxed);
        let mut inherited_ignored = Vec::new();
        for sig in CAUGHT {
            let old = set_handler(sig, on_signal as *const () as libc::sighandler_t)?;
            if old == libc::SIG_IGN {
                inherited_ignored.push(sig);
                if kept_ignored(sig) {
                    set_handler(sig, libc::SIG_IGN)?;
                }
            }
        }
        Ok(Signals {
            read,
            _write: write,
            inherited_ignored,
        })
    }

    /// Wait up to `timeout` for signals and return those that arrived, in order.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> Vec<libc::c_int> {
        let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        let mut pfd = libc::pollfd {
            fd: self.read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one live `pollfd`, count 1. EINTR simply returns early.
        unsafe {
            libc::poll(&raw mut pfd, 1, ms);
        }
        self.drain()
    }

    /// The signals already queued, without waiting (never one kept ignored: see the module
    /// note).
    #[must_use]
    pub fn drain(&self) -> Vec<libc::c_int> {
        let mut out = Vec::new();
        let mut buf = [0_u8; 64];
        loop {
            // SAFETY: reads at most `buf.len()` bytes into the live local buffer.
            let n =
                unsafe { libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            let Ok(n) = usize::try_from(n) else {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            };
            if n == 0 {
                break;
            }
            let Some(read) = buf.get(..n) else { break };
            out.extend(
                read.iter()
                    .map(|b| libc::c_int::from(*b))
                    .filter(|sig| !self.keeps_ignored(*sig)),
            );
        }
        out
    }

    /// Whether `sig` was ignored at start and stays ignored.
    fn keeps_ignored(&self, sig: libc::c_int) -> bool {
        kept_ignored(sig) && self.inherited_ignored.contains(&sig)
    }
}

fn set_flag(fd: RawFd, get: libc::c_int, set: libc::c_int, flag: libc::c_int) -> io::Result<()> {
    // SAFETY: `fcntl` on a descriptor this process owns, with integer arguments only.
    let cur = unsafe { libc::fcntl(fd, get) };
    // SAFETY: as above.
    if cur < 0 || unsafe { libc::fcntl(fd, set, cur | flag) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Install `handler` (a function, `SIG_DFL` or `SIG_IGN`) for `sig` with `signal(3)`'s BSD
/// semantics (on macOS and glibc: `sigaction` restarting interrupted system calls, with
/// nothing masked during it but `sig` itself), and return the handler it replaced.
///
/// # Errors
/// `signal` failed (`SIG_ERR`).
pub fn set_handler(
    sig: libc::c_int,
    handler: libc::sighandler_t,
) -> io::Result<libc::sighandler_t> {
    // SAFETY: `signal` takes a valid signal number and a handler that is `SIG_DFL`, `SIG_IGN`
    // or an `extern "C" fn(c_int)` that does only async-signal-safe work (`on_signal`).
    let old = unsafe { libc::signal(sig, handler) };
    if old == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    Ok(old)
}

/// Stop this process (SIGSTOP), returning once something continues it.
pub fn stop_self() {
    // SAFETY: `raise` with a valid signal number has no memory effects.
    unsafe {
        libc::raise(libc::SIGSTOP);
    }
}

/// Send `sig` to process `pid`, answering whether the kernel took it.
#[must_use]
pub fn send(pid: libc::pid_t, sig: libc::c_int) -> bool {
    // SAFETY: `kill` takes two integers; `pid` is always a positive child pid here.
    pid > 0 && unsafe { libc::kill(pid, sig) } == 0
}

/// End this process the way a child that ended with wait status `status` ended: its exit
/// code, or death by the same signal (with no core file of the harness), so the shell sees
/// what it would have seen run directly.
pub fn end_like(status: libc::c_int) -> ! {
    if libc::WIFSIGNALED(status) {
        let sig = libc::WTERMSIG(status);
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: plain calls with a valid signal number and a live `rlimit`.
        unsafe {
            libc::setrlimit(libc::RLIMIT_CORE, &raw const none);
        }
        let _ = set_handler(sig, libc::SIG_DFL);
        // SAFETY: an all-zero `sigset_t` is valid storage; it is initialized before use.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `sigemptyset` and `sigaddset` write only the live local set.
        unsafe {
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
        }
        let unblock: *const libc::sigset_t = std::ptr::from_ref(&set);
        // SAFETY: `unblock` points at the live set; unblocking a signal and raising it touch
        // no memory of ours.
        unsafe {
            libc::sigprocmask(libc::SIG_UNBLOCK, unblock, std::ptr::null_mut());
            libc::raise(sig);
        }
        std::process::exit(128_i32.saturating_add(sig));
    }
    if libc::WIFEXITED(status) {
        std::process::exit(libc::WEXITSTATUS(status));
    }
    std::process::exit(1)
}
