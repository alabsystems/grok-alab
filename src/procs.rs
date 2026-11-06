// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the harness can read about its child without the screen: whether it runs anything
//! under it, and when its terminal last moved.

use std::io::IsTerminal as _;
use std::os::fd::BorrowedFd;
use std::os::unix::fs::MetadataExt as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The most descendants walked before the answer is "busy" anyway.
const MAX_WALK: usize = 4096;

/// Every live descendant of `pid` (children, their children, …), or `None` when this
/// platform cannot say. A tool command the vendor runs (a shell, a build, a subagent) is one.
#[must_use]
pub fn descendants(pid: libc::pid_t) -> Option<Vec<libc::pid_t>> {
    let mut out: Vec<libc::pid_t> = Vec::new();
    let mut frontier = vec![pid];
    while let Some(p) = frontier.pop() {
        for kid in children(p)? {
            if kid != pid && !out.contains(&kid) {
                out.push(kid);
                frontier.push(kid);
                if out.len() >= MAX_WALK {
                    return Some(out);
                }
            }
        }
    }
    Some(out)
}

/// The direct children of `pid` (macOS: libproc's `proc_listchildpids`).
#[cfg(target_os = "macos")]
fn children(pid: libc::pid_t) -> Option<Vec<libc::pid_t>> {
    /// Room for this many direct children per call.
    const PIDS: usize = 1024;
    /// The buffer's size in bytes, computed when the harness is compiled.
    const BYTES: usize = PIDS * std::mem::size_of::<libc::pid_t>();
    let mut buf: Vec<libc::pid_t> = vec![0; PIDS];
    let bytes = libc::c_int::try_from(BYTES).ok()?;
    // SAFETY: the buffer is live and `bytes` long; the call writes at most that many bytes.
    let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr().cast(), bytes) };
    let n = usize::try_from(n).ok()?;
    // libproc answers a count of pids; were it bytes, the zeroed tail is filtered out below.
    let n = n.min(buf.len());
    Some(buf.get(..n)?.iter().copied().filter(|p| *p > 0).collect())
}

/// The direct children of `pid` (Linux: every `/proc/<pid>/stat` whose parent is `pid`).
#[cfg(target_os = "linux")]
fn children(pid: libc::pid_t) -> Option<Vec<libc::pid_t>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Some(p) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid …`: the comm may hold spaces and parentheses.
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|f| f.parse::<libc::pid_t>().ok());
        if ppid == Some(pid) {
            out.push(p);
        }
    }
    Some(out)
}

/// Anywhere else the harness cannot say, and the answer is "busy".
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn children(_pid: libc::pid_t) -> Option<Vec<libc::pid_t>> {
    None
}

/// How long ago the terminal on `fd` last carried input the program read (its access time)
/// and output the program wrote (its modification time), or `None` when `fd` is no terminal.
/// MEASURED on macOS (a pty, 2026-10-24): a write by the program moves the slave's mtime; a
/// read of typed input moves its atime; input queued and not yet read moves neither. But
/// EVERY `read(2)` of the terminal moves the atime, even one that returns no data (a
/// non-blocking read, a read a timeout interrupts), while `select`, `poll` and `kqueue` waits
/// move nothing: a vendor that reads its terminal on a timer reads here as always typing, and
/// every move holds (fail closed).
///
/// AND macOS STAMPS THEM LAZILY (MEASURED the same day): a write that comes soon after the
/// terminal's last stamp is not stamped when it happens but at the next `stat`, with that
/// stat's time, so a first `stat` long after a write can read it as written "0 ms ago" (8 of
/// 10 trials, a write 1.15 s before the `stat`). The error only ever makes an event look
/// newer, never older; a caller that stats every tick (the supervising loop does) bounds it to
/// one tick.
///
/// The `fstat` is std's (`File::metadata` on a duplicate of `fd`, closed again at once): the
/// same times as an `fstat` of `fd` itself, with no zeroed `stat` the verified lane refuses.
#[must_use]
pub fn tty_quiet(fd: BorrowedFd<'_>, now: SystemTime) -> Option<TtyQuiet> {
    if !fd.is_terminal() {
        return None;
    }
    let st = std::fs::File::from(fd.try_clone_to_owned().ok()?)
        .metadata()
        .ok()?;
    let input = at(st.atime(), st.atime_nsec())?;
    let output = at(st.mtime(), st.mtime_nsec())?;
    // A time in the future (a clock step) reads as "just now": never older than it is.
    let age = |t: SystemTime| now.duration_since(t).unwrap_or(Duration::ZERO);
    Some(TtyQuiet {
        input: age(input),
        output: age(output),
    })
}

/// The time `secs` and `nanos` after the epoch, when it is one.
fn at(secs: i64, nanos: i64) -> Option<SystemTime> {
    let secs = u64::try_from(secs).ok()?;
    let nanos = u32::try_from(nanos).ok()?;
    UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

/// How long the terminal has been quiet in each direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TtyQuiet {
    /// Since the program last read typed input.
    pub input: Duration,
    /// Since the program last wrote output.
    pub output: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawned_child_and_its_child_are_descendants() {
        let mut sh = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30 & wait"])
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sh");
        let me = libc::pid_t::try_from(std::process::id()).unwrap();
        let sh_pid = libc::pid_t::try_from(sh.id()).unwrap();
        let mut seen = None;
        for _ in 0..100 {
            let d = descendants(me).expect("this platform answers");
            if d.contains(&sh_pid) && descendants(sh_pid).unwrap().len() == 1 {
                seen = Some(d);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let d = seen.expect("sh and its sleep were never seen");
        let grandchild = descendants(sh_pid).unwrap()[0];
        assert!(d.contains(&grandchild), "{d:?} lacks {grandchild}");
        assert!(!d.contains(&me));
        assert!(
            crate::signals::send(grandchild, libc::SIGKILL),
            "kill the sleep"
        );
        let _ = sh.wait();
        assert_eq!(
            descendants(sh_pid).unwrap_or_default(),
            Vec::<libc::pid_t>::new()
        );
    }

    #[test]
    fn a_pipe_is_no_terminal() {
        use std::os::fd::AsFd as _;
        let (reader, _writer) = std::io::pipe().unwrap();
        assert_eq!(tty_quiet(reader.as_fd(), SystemTime::now()), None);
    }
}
