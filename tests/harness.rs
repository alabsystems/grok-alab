// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness end to end, against fake grok builds in a fake atpkg store.

mod common;

use std::time::Duration;

use common::{Harness, Pty, Rig, exists, ps_state, signal_pid, wait_until};

const V1: (u64, u64, u64) = (1, 0, 70);
const V2: (u64, u64, u64) = (1, 0, 71);
const V3: (u64, u64, u64) = (1, 0, 72);
const END: Duration = Duration::from_secs(10);

/// The harness's own version, as `--version` and the journal print it (never a literal: a
/// `pub bump` must not turn the suite red).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `grok-alab <VERSION> (grok <vendor>)`, as `--version` prints it.
fn version_line(vendor: &str) -> String {
    format!("grok-alab {VERSION} (grok {vendor})\n")
}

fn exit_code(status: libc::c_int) -> Option<i32> {
    libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
}

fn term_signal(status: libc::c_int) -> Option<i32> {
    libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status))
}

/// A rig with build 1.0.70 current.
fn rig_v1(name: &str) -> (Rig, u64) {
    let rig = Rig::new(name);
    let id = rig.install(V1);
    rig.make_current(id);
    (rig, id)
}

/// Start the harness on a pty with fast timings, and wait for the fake's first start line.
fn start_on_pty(rig: &Rig, args: &[&str], env: &[(&str, &str)]) -> (Harness, Pty) {
    let mut pty = Pty::new();
    let mut cmd = rig.command(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    pty.attach(&mut cmd);
    let h = Harness::spawn(&mut cmd);
    pty.release_slave();
    assert!(
        wait_until(END, || !rig.starts().is_empty()),
        "the fake never started; journal:\n{}",
        rig.journal_text()
    );
    (h, pty)
}

/// Run the harness to its end with no terminal, for a launch that must never start the
/// vendor: the fake is told to exit at once, so a regression fails the test instead of
/// idling it.
fn never_starts(rig: &Rig, args: &[&str]) -> std::process::Output {
    rig.command(args)
        .env("FAKE_GROK_MODE", "exit:0")
        .output()
        .unwrap()
}

/// The harness and its newest child are both still running (the positive control for every
/// "nothing happened" assertion: a fake that died early would make those pass vacuously).
fn still_up(h: &mut Harness, rig: &Rig) {
    assert!(
        h.alive(),
        "the harness ended early; journal:\n{}",
        rig.journal_text()
    );
    let last = rig.starts().last().cloned().expect("a start");
    assert!(
        exists(last.pid) && !ps_state(last.pid).contains('Z'),
        "the child {} is gone; journal:\n{}",
        last.pid,
        rig.journal_text()
    );
}

#[test]
fn refuses_clearly_when_the_vendor_is_not_installed() {
    let rig = Rig::new("missing");
    let out = rig.command(&[]).output().unwrap();
    assert_eq!(out.status.code(), Some(127));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("grok is not installed"), "{err}");
    assert!(err.contains("aterm pkg install grok"), "{err}");
    assert!(
        !rig.log_text().contains("PATH grok ran"),
        "fell back to PATH"
    );

    let out = never_starts(&rig, &["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        version_line("not installed")
    );
}

#[test]
fn refuses_a_build_that_is_not_marked_ready() {
    let (rig, id) = rig_v1("notready");
    std::fs::write(rig.program_dir().join(format!("{id}.ready")), "pending\n").unwrap();
    let out = never_starts(&rig, &[]);
    assert_eq!(out.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not marked ready"));
    assert!(rig.starts().is_empty());
}

#[test]
fn version_names_the_harness_and_the_vendor_build() {
    let (rig, _) = rig_v1("version");
    for flag in ["--version", "-v", "version"] {
        let out = never_starts(&rig, &[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            version_line("1.0.70"),
            "{flag}"
        );
    }
    assert!(rig.starts().is_empty(), "--version must not run the vendor");
}

/// The version is the `.vendor` record's, and only where it names its build: a record that is
/// missing, garbage or names another version says `unknown`, never the version the build id
/// encodes, and never what grok would answer. A store that cannot be resolved says `unknown`
/// too, and `--version` still succeeds.
#[test]
fn version_says_unknown_when_the_record_does_not_name_the_build() {
    let (rig, id) = rig_v1("versionunknown");
    let record = rig.program_dir().join(format!("{id}.vendor"));
    let version = |rig: &Rig| {
        let out = never_starts(rig, &["--version"]);
        assert_eq!(out.status.code(), Some(0));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    for (why, text) in [
        ("garbage", Some("not a record\n")),
        ("no version", Some("schema = 1\nprogram = \"grok\"\n")),
        ("another version", Some("version = \"1.0.71\"\n")),
        ("not canonical", Some("version = \"1.0.070\"\n")),
        ("missing", None),
    ] {
        match text {
            Some(text) => std::fs::write(&record, text).unwrap(),
            None => std::fs::remove_file(&record).unwrap(),
        }
        assert_eq!(version(&rig), version_line("unknown"), "{why}");
    }
    std::fs::write(
        &record,
        "schema = 1\nprogram = \"grok\"\nversion = \"1.0.70\"\n",
    )
    .unwrap();
    assert_eq!(version(&rig), version_line("1.0.70"));
    std::fs::write(rig.program_dir().join(format!("{id}.ready")), "pending\n").unwrap();
    assert_eq!(version(&rig), version_line("unknown"), "not ready");
    let out = rig
        .command(&["--version"])
        .env("GROK_ALAB_PKG_PREFIX", "relative/pkg")
        .env("FAKE_GROK_MODE", "exit:0")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        version_line("unknown"),
        "an unresolved store"
    );
    assert!(rig.starts().is_empty(), "--version must not run the vendor");
}

/// A `HOME` that is not absolute is no home, and the password database's is taken, as before
/// 59797c1 (with which `run` exited 2, "no home directory is known", before reading its
/// arguments). No `GROK_ALAB_PKG_PREFIX` here: the prefix comes from the home. The rig's home
/// holds atpkg's default prefix (its store, build 9.8.7 current), so `--version` says which
/// home was taken: an absolute `HOME` naming it says 9.8.7; a relative `HOME` naming the same
/// directory from the working directory must say what `HOME` unset says (the password
/// database's home, never the rig's).
#[test]
fn a_home_that_is_not_absolute_falls_back_to_the_password_database() {
    use std::ffi::OsStr;
    let rig = Rig::new("relhome");
    let id = rig.install((9, 8, 7));
    // `current` names its build relatively, so it names a build of the store whichever path
    // reaches it (an absolute link must name `<prefix>/store/grok` exactly).
    std::os::unix::fs::symlink(id.to_string(), rig.program_dir().join("current")).unwrap();
    let default_prefix = if cfg!(target_os = "macos") {
        rig.home.join("Library/Application Support/aterm/pkg")
    } else {
        rig.home.join(".local/share/aterm/pkg")
    };
    std::fs::create_dir_all(default_prefix.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&rig.prefix, &default_prefix).unwrap();
    let version = |home: Option<&OsStr>| {
        let mut cmd = rig.command(&["--version"]);
        cmd.env_remove("GROK_ALAB_PKG_PREFIX")
            .env_remove("HOME")
            .env("FAKE_GROK_MODE", "exit:0")
            .current_dir(&rig.root);
        if let Some(h) = home {
            cmd.env("HOME", h);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let rig_home = version(Some(rig.home.as_os_str()));
    assert_eq!(
        rig_home,
        (Some(0), version_line("9.8.7"), String::new()),
        "an absolute HOME is used as it is"
    );
    let unset = version(None);
    assert_eq!(unset.0, Some(0), "{unset:?}");
    assert_ne!(unset.1, version_line("9.8.7"), "the rig's home was taken");
    for home in ["home", "./home", "home/", ""] {
        assert_eq!(version(Some(OsStr::new(home))), unset, "HOME={home:?}");
    }
    assert!(rig.starts().is_empty(), "--version must not run the vendor");
}

#[test]
fn never_execs_itself_even_when_the_store_names_it() {
    let rig = Rig::new("selfexec");
    let id = rig.install(V1);
    let bin = rig
        .program_dir()
        .join(id.to_string())
        .join("bin")
        .join("grok");
    std::fs::remove_file(&bin).unwrap();
    std::os::unix::fs::symlink(common::HARNESS, &bin).unwrap();
    rig.make_current(id);
    let out = rig.command(&[]).output().unwrap();
    assert_eq!(out.status.code(), Some(126));
    assert!(String::from_utf8_lossy(&out.stderr).contains("this harness itself"));
}

/// Make build `id`'s `bin/grok` the shell script `body` (with `@H@` the harness and `@N@` a
/// counter file each run of the script bumps), and answer the counter file.
fn wrap_build(rig: &Rig, id: u64, body: &str) -> std::path::PathBuf {
    let bin = rig
        .program_dir()
        .join(id.to_string())
        .join("bin")
        .join("grok");
    let count = rig.root.join("wrapper-runs");
    std::fs::write(&count, "0").unwrap();
    let script = body
        .replace("@H@", common::HARNESS)
        .replace("@N@", &count.display().to_string());
    std::fs::write(&bin, script).unwrap();
    count
}

fn runs(count: &std::path::Path) -> u32 {
    std::fs::read_to_string(count)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn never_runs_itself_through_a_store_entry_that_execs_it() {
    // The store's grok is a shim that execs the harness: `is_self` cannot see it (another
    // file), so the harness it starts must see that it sits in its parent's vendor slot.
    // The script stops itself after 5 runs, so a failing guard ends the test, not the machine.
    let rig = Rig::new("execshim");
    let id = rig.install(V1);
    let count = wrap_build(
        &rig,
        id,
        "#!/bin/sh\nn=$(cat '@N@'); echo $((n + 1)) > '@N@'\n[ \"$n\" -ge 5 ] && exit 42\nexec '@H@' \"$@\"\n",
    );
    rig.make_current(id);
    let out = rig.command(&["-p", "hi"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(126), "{err}");
    assert!(err.contains("leads back to this harness"), "{err}");
    assert_eq!(runs(&count), 1, "the harness ran itself again: {err}");
}

#[test]
fn a_store_entry_that_runs_the_harness_as_its_child_is_bounded() {
    // A shim that runs the harness as its CHILD (no exec) is a fresh parent each level, so the
    // vendor-slot check cannot see it; the depth bound ends the chain at MAX_DEPTH.
    let rig = Rig::new("childshim");
    let id = rig.install(V1);
    let count = wrap_build(
        &rig,
        id,
        "#!/bin/sh\nn=$(cat '@N@'); echo $((n + 1)) > '@N@'\n[ \"$n\" -ge 20 ] && exit 42\n'@H@' \"$@\"\nexit $?\n",
    );
    rig.make_current(id);
    let out = rig.command(&["-p", "hi"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(126), "{err}");
    assert!(err.contains("harnesses already run above"), "{err}");
    assert_eq!(runs(&count), grok_alab::MAX_DEPTH, "{err}");
}

#[test]
fn passes_every_argument_through_and_exits_with_the_childs_status() {
    let (rig, id) = rig_v1("passthrough");
    let (mut h, _pty) = start_on_pty(
        &rig,
        &["--model", "grok-4", "hello world"],
        &[("FAKE_GROK_MODE", "exit:3")],
    );
    let status = h.wait_end(END);
    assert_eq!(exit_code(status), Some(3), "status {status}");
    let starts = rig.starts();
    assert_eq!(starts.len(), 1);
    let s = &starts[0];
    assert_eq!(s.build, id);
    // A fresh session: the harness put `--session-id <uuid>` in front, then every argument.
    assert_eq!(s.argv[0], "--session-id");
    assert!(
        grok_alab::lanes::is_session_uuid(&s.argv[1]),
        "{:?}",
        s.argv
    );
    assert_eq!(&s.argv[2..], ["--model", "grok-4", "hello world"]);
    assert_eq!(s.autoupdater, "1", "the vendor's self-update stays off");
    assert_eq!(s.harness, h.pid.to_string());
    assert!(!rig.log_text().contains("PATH grok ran"));
    assert!(
        rig.journal_text()
            .contains(&format!("START grok-alab {VERSION} runs grok 1.0.70"))
    );
    assert_eq!(s.depth, "1", "the child is told one harness runs above it");
}

#[test]
fn a_subcommand_runs_exactly_as_typed() {
    let (rig, _) = rig_v1("subcommand");
    let (mut h, _pty) = start_on_pty(&rig, &["sessions", "list"], &[("FAKE_GROK_MODE", "exit:0")]);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
    assert_eq!(rig.starts()[0].argv, ["sessions", "list"]);
}

#[test]
fn grok_update_is_refused_in_favour_of_atpkg() {
    let (rig, _) = rig_v1("update");
    let out = never_starts(&rig, &["update"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("aterm pkg update grok"));
    assert!(rig.starts().is_empty());
}

#[test]
fn grok_update_after_a_prompt_is_refused_too() {
    // clap reads `grok [PROMPT] [COMMAND]`: `grok "fix it" update` runs the self-updater.
    let (rig, _) = rig_v1("updateafterprompt");
    let out = never_starts(&rig, &["fix it", "update"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("aterm pkg update grok"));
    assert!(rig.starts().is_empty());
}

#[test]
fn a_child_killed_by_a_signal_kills_the_harness_by_the_same_signal() {
    let (rig, _) = rig_v1("signaled");
    let (mut h, _pty) = start_on_pty(&rig, &[], &[("FAKE_GROK_MODE", "signal:USR2")]);
    let status = h.wait_end(END);
    assert_eq!(term_signal(status), Some(libc::SIGUSR2), "status {status}");
}

#[test]
fn term_is_passed_on_and_the_session_ends_even_with_an_update_waiting() {
    let (rig, _) = rig_v1("term");
    let (mut h, _pty) = start_on_pty(&rig, &[], &[]);
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    let status = h.wait_end(END);
    assert_eq!(exit_code(status), Some(0));
    let log = rig.log_text();
    assert!(log.contains("term build="), "{log}");
    assert_eq!(rig.starts().len(), 1, "no relaunch after a TERM: {log}");
    assert!(rig.journal_text().contains("PASS TERM passed on"));
}

#[test]
fn hup_is_passed_on() {
    let (rig, _) = rig_v1("hup");
    let (mut h, _pty) = start_on_pty(&rig, &[], &[]);
    still_up(&mut h, &rig);
    h.signal(libc::SIGHUP);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
    assert!(rig.log_text().contains("hup build="));
}

#[test]
fn int_and_quit_belong_to_the_child_and_the_harness_ignores_them() {
    let (rig, _) = rig_v1("int");
    let (mut h, _pty) = start_on_pty(&rig, &[], &[]);
    let child = rig.starts()[0].pid;
    h.signal(libc::SIGINT);
    h.signal(libc::SIGQUIT);
    std::thread::sleep(Duration::from_millis(500));
    assert!(h.alive(), "the harness died of INT/QUIT");
    assert!(exists(child), "the child died");
    assert!(
        !rig.log_text().contains("int build="),
        "the harness passed INT on; the terminal gives the child its own copy"
    );
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

/// A HUP ignored when the harness starts (as `nohup` leaves it) stays ignored: the harness
/// never acts on one (nothing passed on, the session does not end), and its child inherits it
/// ignored (bash cannot trap a signal ignored at its start, so the fake logs nothing and lives).
#[test]
fn a_hup_ignored_at_start_stays_ignored_in_the_harness_and_its_child() {
    use std::os::unix::process::CommandExt as _;
    let (rig, _) = rig_v1("hup-ignored");
    let mut pty = Pty::new();
    let mut cmd = rig.command(&[]);
    // SAFETY: `signal` is async-signal-safe and touches no memory of the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    pty.attach(&mut cmd);
    let mut h = Harness::spawn(&mut cmd);
    pty.release_slave();
    assert!(
        wait_until(END, || !rig.starts().is_empty()),
        "the fake never started; journal:\n{}",
        rig.journal_text()
    );
    let child = rig.starts()[0].pid;
    h.signal(libc::SIGHUP);
    signal_pid(child, libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(500));
    still_up(&mut h, &rig);
    assert!(
        !rig.log_text().contains("hup build="),
        "the child caught a HUP it should have inherited ignored"
    );
    let journal = rig.journal_text();
    assert!(!journal.contains("HUP passed on"), "{journal}");
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn tstp_is_followed_and_cont_resumes_both() {
    let (rig, _) = rig_v1("tstp");
    let mut pty = Pty::new();
    let mut cmd = rig.command(&[]);
    pty.attach(&mut cmd);
    let mut h = Harness::spawn_in_own_group(&mut cmd);
    pty.release_slave();
    assert!(wait_until(END, || !rig.starts().is_empty()));
    let child = rig.starts()[0].pid;
    // As the terminal does on ^Z: TSTP to the whole foreground group.
    h.signal_group(libc::SIGTSTP);
    assert!(
        h.wait_stopped(END),
        "the harness did not stop with its child"
    );
    assert!(
        ps_state(child).contains('T'),
        "child state {}",
        ps_state(child)
    );
    assert!(rig.journal_text().contains("STOP grok stopped"));
    // As the shell does on `fg`: CONT to the group.
    h.signal_group(libc::SIGCONT);
    assert!(
        wait_until(END, || !ps_state(child).contains('T')
            && !ps_state(h.pid).contains('T')),
        "child {} harness {}",
        ps_state(child),
        ps_state(h.pid)
    );
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("CONT continued")));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

/// Fast gates for the move tests: 300 ms of output and input quiet.
const FAST: [(&str, &str); 2] = [
    ("GROK_ALAB_QUIET_MS", "300"),
    ("GROK_ALAB_HUMAN_GRACE_MS", "300"),
];

#[test]
fn a_newer_build_takes_over_at_a_safe_point_with_the_resume_argv_and_the_harness_never_exits() {
    let (rig, id1) = rig_v1("relaunch");
    let (mut h, _pty) = start_on_pty(&rig, &["--always-approve", "first prompt"], &FAST);
    let first = rig.starts()[0].clone();
    assert_eq!(first.build, id1);
    assert_eq!(first.argv[0], "--session-id");
    let session = first.argv[1].clone();
    assert_eq!(&first.argv[2..], ["--always-approve", "first prompt"]);

    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);

    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch; journal:\n{}",
        rig.journal_text()
    );
    let log = rig.log_text();
    assert!(log.contains(&format!("term build={id1}")), "{log}");
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id2);
    // The kept flags, then the vendor's resume pair; the first prompt is never sent again.
    assert_eq!(
        second.argv,
        ["--always-approve", "--resume", session.as_str()]
    );
    // Same harness process all along.
    assert!(h.alive());
    assert_eq!(second.harness, h.pid.to_string());
    assert_eq!(first.harness, second.harness);
    let journal = rig.journal_text();
    assert!(
        journal.contains("MOVE safe point: SIGTERM to grok 1.0.70"),
        "{journal}"
    );
    assert!(
        journal.contains("RELAUNCHED grok 1.0.70 -> 1.0.71"),
        "{journal}"
    );
    assert!(!exists(first.pid) || ps_state(first.pid).contains('Z'));

    // `--version` now names the new build, and the session ends with the new child.
    let out = never_starts(&rig, &["--version"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), version_line("1.0.71"));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
    assert!(rig.log_text().contains(&format!("term build={id2}")));
}

#[test]
fn without_a_safe_point_request_a_newer_build_waits() {
    let (rig, _) = rig_v1("norequest");
    let (mut h, _pty) = start_on_pty(&rig, &[], &FAST);
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(rig.starts().len(), 1, "moved with no request");
    assert!(!rig.log_text().contains("term build="));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn never_relaunches_while_a_tool_process_runs_under_grok() {
    let (rig, _) = rig_v1("busychild");
    let (mut h, _pty) = start_on_pty(
        &rig,
        &[],
        &[FAST[0], FAST[1], ("FAKE_GROK_MODE", "busy-child")],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("HOLD safe point refused")));
    let journal = rig.journal_text();
    assert!(journal.contains("process(es) run under grok"), "{journal}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !rig.log_text().contains("term build="),
        "a busy child was ended"
    );
    assert_eq!(rig.starts().len(), 1);
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn never_relaunches_while_grok_is_drawing() {
    let (rig, _) = rig_v1("busyoutput");
    // The fake draws a dot every second; quiet needs three.
    let (mut h, _pty) = start_on_pty(
        &rig,
        &[],
        &[
            ("GROK_ALAB_QUIET_MS", "3000"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "300"),
            ("FAKE_GROK_MODE", "busy-output"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("HOLD safe point refused")));
    let journal = rig.journal_text();
    assert!(journal.contains("wrote to the terminal"), "{journal}");
    assert!(!rig.log_text().contains("term build="));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn never_relaunches_while_someone_is_typing() {
    let (rig, id1) = rig_v1("typing");
    let grace = Duration::from_millis(1500);
    let (mut h, pty) = start_on_pty(
        &rig,
        &[],
        &[
            ("GROK_ALAB_QUIET_MS", "300"),
            ("GROK_ALAB_HUMAN_GRACE_MS", "1500"),
        ],
    );
    let fake = rig.starts()[0].pid;
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    // Let the input quiet pass the grace first (the idle fake never reads its terminal), so
    // only the typing below can hold the move.
    std::thread::sleep(grace + Duration::from_millis(300));
    pty.type_input(b"half a thought\n");
    // As a TUI does once its terminal polls readable: the fake reads the typed line.
    signal_pid(fake, libc::SIGUSR2);
    assert!(
        wait_until(END, || rig.log_text().contains("line=half a thought")),
        "the fake never read the line: {}",
        rig.log_text()
    );
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("HOLD safe point refused")),
        "journal:\n{}\nlog:\n{}",
        rig.journal_text(),
        rig.log_text()
    );
    let journal = rig.journal_text();
    assert!(journal.contains("typed"), "{journal}");
    assert!(!rig.log_text().contains("term build="));
    still_up(&mut h, &rig);
    // The control: nothing changes but the clock, and once the grace has passed with nothing
    // more typed the same request moves. The hold above was the typing, nothing else.
    std::thread::sleep(grace + Duration::from_millis(300));
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no move after the grace; journal:\n{}",
        rig.journal_text()
    );
    assert!(rig.log_text().contains(&format!("term build={id1}")));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn a_child_that_ignores_the_harness_term_is_never_killed_harder() {
    let (rig, _) = rig_v1("ignoreterm");
    let (mut h, _pty) = start_on_pty(
        &rig,
        &[],
        &[
            FAST[0],
            FAST[1],
            ("GROK_ALAB_EXIT_GRACE_MS", "500"),
            ("FAKE_GROK_MODE", "ignore-term"),
        ],
    );
    let child = rig.starts()[0].pid;
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig.log_text().contains("term-ignored")));
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("did not exit within 500 ms")),
        "{}",
        rig.journal_text()
    );
    // Well past the grace: the child still runs on its build, was sent nothing harder, and
    // no second grok started.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        exists(child) && !ps_state(child).contains('Z'),
        "the child was killed"
    );
    assert_eq!(
        rig.log_text().matches("term-ignored").count(),
        1,
        "TERM sent again"
    );
    assert_eq!(rig.starts().len(), 1);
    still_up(&mut h, &rig);
    h.signal(libc::SIGHUP);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

/// A late answer to the harness's SIGTERM finishes the move: grok exits 0 two to three seconds
/// after it (past the 300 ms grace), nobody typed since, so the session is relaunched on the new
/// build with its resume argv, and nothing is left for the person to do.
#[test]
fn a_child_that_honours_the_term_after_the_grace_is_moved_when_nobody_typed_since() {
    let (rig, id1) = rig_v1("lateterm");
    let only_v1 = format!("FAKE_GROK_MODE_{id1}");
    let (mut h, pty) = start_on_pty(
        &rig,
        &[],
        &[
            FAST[0],
            FAST[1],
            ("GROK_ALAB_EXIT_GRACE_MS", "300"),
            (only_v1.as_str(), "slow-term:3"),
        ],
    );
    let session = rig.starts()[0].argv[1].clone();
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("did not exit within 300 ms")));
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "the late exit ended the session; journal:\n{}",
        rig.journal_text()
    );
    assert!(rig.log_text().contains(&format!("term build={id1}")));
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id2);
    assert_eq!(second.argv, ["--resume", session.as_str()]);
    assert!(
        rig.journal_text()
            .contains("RELAUNCHED grok 1.0.70 -> 1.0.71 as pid"),
        "{}",
        rig.journal_text()
    );
    assert!(rig.journal_text().contains("after a late exit"));
    still_up(&mut h, &rig);
    let screen = String::from_utf8_lossy(&pty.output.lock().unwrap()).into_owned();
    assert!(!screen.contains("--resume"), "{screen}");
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

/// THE CONTROL: the same late exit after a person typed (the fake read a typed line after the
/// harness's SIGTERM) may be their quit, so the session ends there, and never in silence.
#[test]
fn a_late_exit_after_someone_typed_ends_the_session_and_says_how_to_resume() {
    let (rig, _) = rig_v1("lateterm-typed");
    let (mut h, pty) = start_on_pty(
        &rig,
        &[],
        &[
            FAST[0],
            FAST[1],
            ("GROK_ALAB_EXIT_GRACE_MS", "300"),
            ("FAKE_GROK_MODE", "slow-term:3"),
        ],
    );
    let first = rig.starts()[0].clone();
    let session = first.argv[1].clone();
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("did not exit within 300 ms")));
    pty.type_input(b"/exit\n");
    signal_pid(first.pid, libc::SIGUSR2);
    assert!(wait_until(END, || rig.log_text().contains("line=/exit")));
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
    assert_eq!(rig.starts().len(), 1, "{}", rig.journal_text());
    let hint = format!("grok --resume {session}");
    assert!(
        wait_until(END, || String::from_utf8_lossy(&pty.output.lock().unwrap())
            .contains(&hint)),
        "terminal: {:?}",
        String::from_utf8_lossy(&pty.output.lock().unwrap())
    );
    assert!(
        rig.journal_text().contains("a person may have typed since"),
        "{}",
        rig.journal_text()
    );
}

/// A NEW BUILD THAT REFUSES TO START GOES BACK: the build a move started exits 3 at once, so the
/// session is resumed on the build it moved from (aterm's harness design §4.3 step 6), and that
/// build is never moved onto again in the session.
#[test]
fn a_move_onto_a_build_that_refuses_to_start_resumes_on_the_previous_build() {
    let (rig, id1) = rig_v1("refused-move");
    let id2 = common::vendor_id(V2.0, V2.1, V2.2);
    let only_v2 = format!("FAKE_GROK_MODE_{id2}");
    let (mut h, _pty) = start_on_pty(&rig, &[], &[FAST[0], FAST[1], (only_v2.as_str(), "exit:3")]);
    let session = rig.starts()[0].argv[1].clone();
    assert_eq!(rig.install(V2), id2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig.starts().len() == 3),
        "no fallback; journal:\n{}",
        rig.journal_text()
    );
    let starts = rig.starts();
    assert_eq!(starts[1].build, id2);
    assert_eq!(starts[2].build, id1, "back on the build it ran");
    assert_eq!(starts[2].argv, ["--resume", session.as_str()]);
    assert!(
        rig.journal_text()
            .contains("FAILED the move onto grok 1.0.71 failed: it ended (exit 3)"),
        "{}",
        rig.journal_text()
    );
    still_up(&mut h, &rig);
    // The failed build is not moved onto again.
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("HOLD safe point refused: no newer grok build")));
    assert_eq!(rig.starts().len(), 3);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

/// A MOVE IS HELD WHILE GROK WOULD START SIGNED OUT: the relaunch would draw xAI's device sign-in
/// and open it in a browser, so the request is refused and grok runs on.
#[test]
fn no_move_while_grok_would_start_signed_out() {
    let (rig, _) = rig_v1("move-signed-out");
    let (mut h, _pty) = start_on_pty(&rig, &[], &FAST);
    let id2 = rig.install(V2);
    rig.make_current(id2);
    rig.sign_out();
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig.journal_text().contains(
            "HOLD safe point refused: grok 1.0.71 would start signed out"
        )),
        "{}",
        rig.journal_text()
    );
    assert!(!rig.log_text().contains("term build="));
    assert_eq!(rig.starts().len(), 1);
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn after_an_unanswered_term_a_later_move_still_works_and_a_later_quit_is_not_blamed_on_it() {
    let (rig, id1) = rig_v1("termonce");
    let (mut h, pty) = start_on_pty(
        &rig,
        &[],
        &[
            FAST[0],
            FAST[1],
            ("GROK_ALAB_EXIT_GRACE_MS", "500"),
            ("FAKE_GROK_MODE", "term-once"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("did not exit within 500 ms")),
        "journal:\n{}\nlog:\n{}\nterminal: {:?}",
        rig.journal_text(),
        rig.log_text(),
        String::from_utf8_lossy(&pty.output.lock().unwrap())
    );
    // A second request: the first child honours this TERM, and the session moves.
    h.signal(libc::SIGUSR1);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch; journal:\n{}",
        rig.journal_text()
    );
    assert!(rig.log_text().contains(&format!("term build={id1}")));
    let second = rig.starts()[1].clone();
    assert_eq!(second.build, id2);
    still_up(&mut h, &rig);
    // The new grok ends on its own (as the person's quit would): no resume hint, since no
    // SIGTERM of the harness ended it.
    signal_pid(second.pid, libc::SIGHUP);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
    std::thread::sleep(Duration::from_millis(200));
    let screen = String::from_utf8_lossy(&pty.output.lock().unwrap()).into_owned();
    assert!(!screen.contains("--resume"), "{screen}");
    assert!(!rig.journal_text().contains("unanswered SIGTERM"));
}

#[test]
fn never_moves_onto_a_build_that_is_the_harness_itself() {
    let (rig, _) = rig_v1("selfmove");
    let (mut h, _pty) = start_on_pty(&rig, &[], &FAST);
    let id2 = rig.install(V2);
    let bin = rig
        .program_dir()
        .join(id2.to_string())
        .join("bin")
        .join("grok");
    std::fs::remove_file(&bin).unwrap();
    std::os::unix::fs::symlink(common::HARNESS, &bin).unwrap();
    rig.make_current(id2);
    assert!(
        wait_until(END, || rig
            .journal_text()
            .contains("is this harness itself")),
        "{}",
        rig.journal_text()
    );
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("HOLD safe point refused: no newer grok build")));
    assert!(!rig.log_text().contains("term build="));
    assert_eq!(rig.starts().len(), 1);
    // Only this harness ever wrote the journal: no second harness started.
    let journal = rig.journal_text();
    let pids: std::collections::BTreeSet<&str> = journal
        .lines()
        .filter_map(|l| l.split(' ').nth(1))
        .collect();
    assert_eq!(pids.len(), 1, "{}", rig.journal_text());
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn a_build_that_turns_out_to_be_the_harness_mid_move_is_skipped_for_the_target() {
    // The move targets 1.0.71; while grok takes a second or two to exit, `current` flips to a 1.0.72
    // that is the harness itself. The relaunch goes to 1.0.71, never to the harness.
    let (rig, _) = rig_v1("selfmidmove");
    let (mut h, _pty) = start_on_pty(
        &rig,
        &[],
        &[
            FAST[0],
            FAST[1],
            ("GROK_ALAB_EXIT_GRACE_MS", "5000"),
            ("FAKE_GROK_MODE", "slow-term:2"),
        ],
    );
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig.log_text().contains("term-slow")));
    let id3 = rig.install(V3);
    let bin = rig
        .program_dir()
        .join(id3.to_string())
        .join("bin")
        .join("grok");
    std::fs::remove_file(&bin).unwrap();
    std::os::unix::fs::symlink(common::HARNESS, &bin).unwrap();
    rig.make_current(id3);
    assert!(
        wait_until(END, || rig.starts().len() == 2),
        "no relaunch; journal:\n{}",
        rig.journal_text()
    );
    assert_eq!(rig.starts()[1].build, id2, "{}", rig.journal_text());
    assert!(
        rig.journal_text()
            .contains("RELAUNCHED grok 1.0.70 -> 1.0.71")
    );
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn a_launch_the_harness_cannot_carry_is_run_as_typed_and_never_relaunched() {
    let (rig, _) = rig_v1("uncarried");
    let (mut h, _pty) = start_on_pty(&rig, &["--continue"], &FAST);
    assert_eq!(rig.starts()[0].argv, ["--continue"], "nothing injected");
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("HOLD safe point refused")));
    assert!(rig.journal_text().contains("cannot be carried"));
    assert!(!rig.log_text().contains("term build="));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
}

#[test]
fn with_no_terminal_the_harness_never_relaunches() {
    let (rig, _) = rig_v1("noterminal");
    let mut cmd = rig.command(&[]);
    for (k, v) in FAST {
        cmd.env(k, v);
    }
    // A pipe for stdin that never yields: the fake idles; there is no terminal to read.
    let mut sh = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 30"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    cmd.stdin(sh.stdout.take().unwrap())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut h = Harness::spawn(&mut cmd);
    assert!(wait_until(END, || !rig.starts().is_empty()));
    let id2 = rig.install(V2);
    rig.make_current(id2);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("SEEN grok 1.0.71")));
    std::thread::sleep(Duration::from_millis(700));
    h.signal(libc::SIGUSR1);
    assert!(wait_until(END, || rig
        .journal_text()
        .contains("HOLD safe point refused")));
    assert!(rig.journal_text().contains("no terminal"));
    still_up(&mut h, &rig);
    h.signal(libc::SIGTERM);
    assert_eq!(exit_code(h.wait_end(END)), Some(0));
    let _ = sh.kill();
    let _ = sh.wait();
}

/// Lays a fake `aterm` on the rig's PATH that records each call and exits
/// `$FAKE_ATERM_EXIT` (default 0); returns the calls file.
fn fake_aterm(rig: &Rig) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let calls = rig.root.join("aterm.calls");
    let aterm = rig.path_dir.join("aterm");
    std::fs::write(
        &aterm,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit \"${{FAKE_ATERM_EXIT:-0}}\"\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).unwrap();
    calls
}

/// One launch to its end with no terminal under aterm (`ATERM_CHILD=1`), the fake grok told
/// to exit at once.
fn under_aterm(rig: &Rig, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = rig.command(args);
    cmd.env("ATERM_CHILD", "1").env("FAKE_GROK_MODE", "exit:0");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

#[test]
fn under_aterm_a_signed_out_interactive_launch_is_signed_in_by_aterm_first() {
    let (rig, _) = rig_v1("signin");
    // The rig starts signed in (its login file); this test starts signed out.
    rig.sign_out();
    let calls = fake_aterm(&rig);
    let called = || std::fs::read_to_string(&calls).unwrap_or_default();
    let signin = "harness signin --cli grok --wait\n";

    // aterm's sign-in fails: exit 69, one line, and grok never runs.
    let out = under_aterm(&rig, &[], &[("FAKE_ATERM_EXIT", "1")]);
    assert_eq!(out.status.code(), Some(69));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("grok was not started"), "{err}");
    assert_eq!(called(), signin);
    assert!(rig.starts().is_empty(), "grok ran: {}", rig.log_text());
    assert!(
        rig.journal_text()
            .contains("REFUSED not signed in to Grok Build")
    );

    // aterm signs the machine in: grok runs after it, once.
    std::fs::remove_file(&calls).unwrap();
    let out = under_aterm(&rig, &["fix it"], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(called(), signin);
    assert_eq!(rig.starts().len(), 1);
    assert!(rig.starts()[0].argv.contains(&"fix it".to_string()));
    assert!(rig.journal_text().contains("NOTE signed in to Grok Build"));

    // A subcommand that opens grok's screen, or may, is gated like a session: `dashboard`
    // (the Agent Dashboard), `sessions` (it restores one), `models` and an unknown verb.
    for args in [
        &["dashboard"][..],
        &["sessions"][..],
        &["models"][..],
        &["agent", "stdio"][..],
    ] {
        std::fs::remove_file(&calls).unwrap_or_default();
        let before = rig.starts().len();
        let out = under_aterm(&rig, args, &[("FAKE_ATERM_EXIT", "1")]);
        assert_eq!(out.status.code(), Some(69), "{args:?}: {out:?}");
        assert_eq!(called(), signin, "{args:?}");
        assert_eq!(rig.starts().len(), before, "{args:?}: grok ran");
    }

    // A one-shot option or a local subcommand never asks aterm, and runs.
    std::fs::remove_file(&calls).unwrap();
    for args in [
        &["login"][..],
        &["logout"][..],
        &["--help"][..],
        &["-p", "hi"][..],
        &["mcp", "list"][..],
        &["doctor"][..],
    ] {
        let before = rig.starts().len();
        let out = under_aterm(&rig, args, &[("FAKE_ATERM_EXIT", "1")]);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert_eq!(rig.starts().len(), before + 1, "{args:?}");
    }
    // Signed in by the environment, or outside aterm or with the harness taken away: no ask.
    for env in [
        &[("XAI_API_KEY", "k"), ("FAKE_ATERM_EXIT", "1")][..],
        &[("GROK_AUTH", "k"), ("FAKE_ATERM_EXIT", "1")][..],
        &[("ATERM_NO_HARNESS", "1"), ("FAKE_ATERM_EXIT", "1")][..],
    ] {
        let out = under_aterm(&rig, &[], env);
        assert_eq!(out.status.code(), Some(0), "{env:?}: {out:?}");
    }
    let out = rig
        .command(&[])
        .env("FAKE_GROK_MODE", "exit:0")
        .env("FAKE_ATERM_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "outside aterm: {out:?}");
    // Signed in by grok's own login file: no ask.
    std::fs::create_dir_all(rig.home.join(".grok")).unwrap();
    let auth = rig.home.join(".grok").join("auth.json");
    std::fs::write(&auth, "{}").unwrap();
    let out = under_aterm(&rig, &[], &[("FAKE_ATERM_EXIT", "1")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(called(), "", "aterm was asked when it must not be");
    // A login grok refused (aterm's `auth.json.refused`, at least as new) is signed out: aterm
    // is asked, and a refusal runs nothing. A newer login signs it in again.
    let marker = rig.home.join(".grok").join("auth.json.refused");
    std::fs::write(&marker, "").unwrap();
    let marked = std::fs::metadata(&marker).unwrap().modified().unwrap();
    let set = |at: std::time::SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(&auth)
            .unwrap()
            .set_modified(at)
            .unwrap();
    };
    set(marked - Duration::from_secs(5));
    let before = rig.starts().len();
    let out = under_aterm(&rig, &[], &[("FAKE_ATERM_EXIT", "1")]);
    assert_eq!(out.status.code(), Some(69), "{out:?}");
    assert_eq!(called(), signin);
    assert_eq!(rig.starts().len(), before);
    std::fs::remove_file(&calls).unwrap();
    set(marked + Duration::from_secs(5));
    let out = under_aterm(&rig, &[], &[("FAKE_ATERM_EXIT", "1")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(called(), "", "a newer login is signed in");
    std::fs::remove_file(&marker).unwrap();

    // No aterm on PATH: exit 69, one line, grok never runs.
    std::fs::remove_file(rig.home.join(".grok").join("auth.json")).unwrap();
    std::fs::remove_file(rig.path_dir.join("aterm")).unwrap();
    let before = rig.starts().len();
    let out = under_aterm(&rig, &[], &[]);
    assert_eq!(out.status.code(), Some(69));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not on PATH"));
    assert_eq!(rig.starts().len(), before);
}
