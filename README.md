# grok-alab

The Grok Build harness for aterm. `grok-alab` runs xAI's **unmodified** `grok`, the build
aterm's package manager (atpkg) installed and verified, as its child on your terminal, with
every argument passed through. When atpkg installs a newer `grok` while a session runs, the
harness moves the session onto the new build at a safe point, with the vendor's own resume
arguments, and the harness itself never exits until the session really ends.

```text
$ grok-alab --version
grok-alab <version> (grok <vendor version>)
$ grok-alab run -- --model grok-code-fast-1 "fix the failing test"
```

The harness's version is its own (`MAJOR.MINOR.0`, `[workspace.package] version` in
`Cargo.toml`), independent of the vendor's: the line above names both.

Under aterm it holds the session under aterm's **harness contract 3**: it claims
`grok-alab@<pid> contract=3 scope=relaunch,upgrade`, follows aterm's harness policy (giving the
claim back under Autopilot Off), relaunches a `grok` that crashed on the same session, and is
built to take the safe point for a move from aterm's read of the screen. **That read cannot pass
on any aterm that ships today**: aterm reports `agent=unknown` for every Grok session (its Grok
reader is not measured) and no `composer=`, so a move under aterm still waits for `SIGUSR1` or
the next launch (see "What it does not do (yet)"). An aterm that does not serve contract 3, or
refuses the claim, gets plain `grok`: the harness execs it in its own place and is gone.

## What it does

1. **Finds `grok` in atpkg's store, and nowhere else.** It runs
   `<prefix>/store/grok/current/bin/grok`, where `<prefix>` is
   `~/Library/Application Support/aterm/pkg` on macOS and `~/.local/share/aterm/pkg`
   elsewhere, or `[packages] prefix` from `aterm.toml`. It never looks on `PATH` and never
   through atpkg's `agents/` twin or `bin/` shim, either of which may lead back to this
   harness. It refuses a store entry that is the harness itself, on the first launch and
   for every move, and an entry that leads back to it (a shim that execs or runs
   `grok-alab`; exit 126, Decision 1). A build
   that is missing, not marked ready (`<build>.ready`), or has no executable `bin/grok` is
   refused with a message naming the fix (`aterm pkg install grok`), exit 127. It applies
   the build's `<build>.shim-env` as atpkg does and always sets
   `GROK_DISABLE_AUTOUPDATER=1`, so the vendor never replaces the build atpkg verified.
2. **Runs it as a child on the inherited terminal.** The child shares the harness's process
   group, so the terminal's own keys reach it directly; the harness proxies nothing and
   writes nothing to the terminal while the child runs. Every argument is passed through as
   typed, with one addition for an interactive session (Decision 3).
3. **Forwards signals and ends as the child ended.** See the table below. The harness
   exits with the child's exit code; a child killed by a signal kills the harness with the
   same signal (core dumps off), so a shell sees what it would have seen running `grok`.
4. **Moves a session onto a newer build.** It reads the store's `current` link once a
   second. When a newer, ready build appears it journals it and waits. At a safe point
   (aterm's read under a claim, or a `SIGUSR1` request; see below) that passes every gate, it
   sends the child `SIGTERM` (Grok Build 1.0.70 restores the terminal and exits 0 on it,
   measured) and relaunches the new build with the session's resume arguments, in the same
   harness process. A new build that refuses to start (it ends by a code other than 0, or a
   fault, within 10 s) is left: the session is resumed on the build it moved from, and that
   build is not moved onto again in the session (aterm's design §4.3 step 6).
5. **Prints both versions.** `grok-alab --version` (also `-v` and `version`, the spellings
   `grok` answers) prints `grok-alab <version> (grok <vendor version>)` without running `grok`.
   After `run`, every word is grok's: `grok-alab run -- --version` is grok's own version.
6. **Under aterm, claims first, then spawns, else gets out of the way** (aterm's
   `docs/DESIGN-harness-repos-2026-10-24.md` §3.2, §8). In an aterm tab (`TERM_PROGRAM=aterm`
   or `ATERM_CHILD=1`) it finds the session's control socket (`ATERM_PARENT_SESSION_ID`, the
   rendezvous directory's `graph/<sid>`, the token beside the socket), reads
   `harness policy`, and goes on only when its `contract=` set lists `3` and `effective=on`.
   It then claims `meta set supervisor grok-alab@<pid> contract=3 scope=relaunch,upgrade
   version=<its version> vendor=<grok's>` and confirms it with `status`
   (`supervisor=grok-alab@<pid> supervisor_proven=1`). aterm proves the claim itself: the
   claim's connection is the harness's own, the harness leads the tab's foreground process
   group, and its executable is a build of `grok-alab` from atpkg's store or its dev link.
   Only then does `grok` start, as its child. Anything else (the person's `--no-alab-harness`
   on the twin, which sets `ATERM_NO_HARNESS=1`; a launch the harness cannot carry; no socket;
   an aterm without contract 3; the policy off; a refused or unconfirmed claim) **execs `grok`
   in the harness's place**, with the launch's own arguments and the build's shim environment:
   the tab runs `grok` under its own name, exactly as if no harness existed. The claim's
   connection stays open for the harness's life (the claim is bound to it), and is watched on
   every tick: when aterm closes it (a restart, a seamless update), the claim is made again at
   once, then every 5 s, and no move or relaunch is taken meanwhile. A second connection
   subscribes to the session's events (`subscribe @<sid> events`) and re-reads `harness policy`
   on `EVENT <sid> harness-policy` (aterm's design §2), or every 5 s where aterm gives no event
   stream. When the policy goes off for the session (Autopilot Off, `[harness] enabled =
   false`), the claim is given back (`meta unset supervisor holder=grok-alab@<pid>`) and made
   again once it is back on.
7. **Relaunches a crashed `grok`** under a proven claim with `[harness] relaunch = true`, both
   read FRESH from aterm when the crash happens (never the policy the claim was made under): a
   child that died of a fault or an abort (`SEGV`, `BUS`, `ILL`, `ABRT`, `FPE`, `TRAP`, `SYS`)
   after running at least 10 s is relaunched on the same session with the resume arguments,
   at most 3 times, after a pause of 1 s that doubles each time. An exit by code, and `KILL`,
   `TERM`, `HUP`, `INT` or `QUIT`, is the session's end. The terminal's modes, taken at start,
   are put back after every child end while the harness owns the terminal.
8. **Never starts a signed-out `grok` itself.** Every relaunch (a crash, a move, a fallback) is
   the harness's own act, and Grok Build started signed out opens xAI's device sign-in in the
   desk's browser. So a relaunch, and the move that would need one, happens only when grok
   would start signed in, read as aterm's row for Grok reads it: a non-empty `GROK_AUTH` or
   `XAI_API_KEY`, else the file `GROK_AUTH_PATH` names, else `${GROK_HOME:-~/.grok}/auth.json`
   (non-empty, and not refused: aterm marks a login it saw grok refuse with `<file>.refused`).
   Otherwise the move is held; a relaunch under aterm asks aterm to sign the machine in first
   (item 9) and follows once it has; else the session ends with one line saying why and how to
   resume once signed in.
9. **Under aterm, signs the machine in once before a signed-out session starts** (Decision 26).
   In an aterm tab (`ATERM_CHILD=1`), before a launch claims or passes through, and before a
   relaunch item 8 would refuse, a signed-out machine (read as item 8 reads it) runs
   `aterm harness signin --cli grok --wait` in the foreground: aterm signs the machine in once,
   in a sandbox that stops the vendor's browser open, with one approval need (code and page)
   for every session that waits on it. `grok` starts only once that exits 0; otherwise a first
   start exits 69 with one line, and a relaunch ends as item 8 says, both journaling `REFUSED`.
   `--help`, `--version`, `-p` and the local subcommands (`login`, `logout`, `mcp`, `doctor`,
   ...) never ask; `dashboard`, `sessions` and every other subcommand do. Nothing changes
   outside aterm or under `--no-alab-harness` (`ATERM_NO_HARNESS`).

### Signals

| Signal the harness gets | What it does |
|---|---|
| `INT`, `QUIT` | Ignores it: the terminal delivered the same signal to `grok`, which owns it. |
| `TSTP` (Ctrl-Z) | Follows the child: when `grok` stops, the harness stops itself, and on `CONT` continues `grok`. |
| `HUP`, `TERM` | Passes it to `grok`; the session ends with it, and no move starts after it. |
| `CONT` | Passes it to `grok`. |
| `USR1` | A safe-point request (below); under an aterm claim, an override of aterm's read. |

A signal the harness inherited as ignored (`nohup`, a background job) stays ignored, for
the child too.

### The safe point

The harness cannot see the screen, so it cannot tell by itself that a turn has ended and
nobody is mid-sentence. A move needs a **witness** for what only the screen shows, and every
**gate**:

* **Under an aterm claim, aterm's read is the witness**, taken on every tick a newer build
  waits, with no signal from anyone: one fresh `status` and `harness policy`, and every one of
  these must hold (aterm's harness design §4.2), a field aterm does not report holding the
  move: the claim is this harness's and proven; the policy is on, `[harness] upgrade = true`
  and `upgrade_ask = false`; **`agent=idle`** (a turn over, no box, question, wall or
  survey; aterm's `prompt` is a permission box and holds, Decision 14); **`composer=empty` or
  `placeholder`** (a draft holds; an aterm that does not report `composer=` holds, Decision
  15); **`human_ms`** is `-` or at least `human_grace_s`; `hand=-`, `hold=0`,
  `input=clear` and `need=-`. Each new reason it holds for is journaled once (`WAIT`).
* **A request is `SIGUSR1` to the harness**: its sender vouches for the screen, and the
  terminal's input quiet (below) is checked in place of aterm's read. Outside aterm it is the
  only witness; under a claim it is the person's override (also of `upgrade_ask = true`).
  The harness's pid is in the child's environment as `GROK_ALAB_PID` and in the journal's
  `START` line:

  ```sh
  kill -USR1 <grok-alab pid>
  ```

  Without a witness a newer build simply waits for the next launch.
* The gates, each checked by the harness and each holding the move when it fails or cannot
  be read: the session is not ending and no move is under way; the launch is one the
  harness can resume (Decision 3); a newer build is installed and ready; **the new build would
  start signed in** (above, 8); **no process runs
  under `grok`** (a tool command, a subagent or a helper); the terminal **wrote nothing for
  20 s** (aterm's own idle settle, `QUIET_S` in aterm's
  `crates/aterm-agent/src/harness/upgrade.rs`); and, for a request, the terminal **read
  nothing typed for 120 s** (aterm's `[harness] human_grace_s` default,
  `crates/aterm-agent/src/supervise/config.rs`).

A refused request is dropped and journaled with the gate that refused it; the sender asks
again. A child that does not exit within 10 s of the harness's `SIGTERM` is left running on
its build and the move is abandoned. The harness never kills a child harder, and never ends
a busy one. A `SIGTERM` cannot be taken back, though: if `grok` honours it after the 10 s, the
harness finishes the move when that exit reads as the SIGTERM's answer and nobody else's: it
ends as grok ends on a SIGTERM (exit 0, or `TERM`), within 60 s of it, and nobody typed since
(the terminal's input clock, and aterm's `human_ms` under a claim, both older than the
SIGTERM). Any other exit after it may be the person's own quit, so the session ends there, and
the harness prints and journals the resume line (`grok --resume <id>`), never in silence.

The terminal clocks are the slave's access time (input the program read) and modification
time (output it wrote). Measured on macOS (2026-10-24, a pty): **every** `read(2)` of the
terminal moves the access time, even one that returns no data (a non-blocking read, a read a
timeout interrupts), while `select`, `poll` and `kqueue` waits move nothing. A vendor that
waits on its terminal with `poll`/`kqueue` and reads only when input is there gives a true
input clock; one that reads on a timer makes every request read as "someone typed" and
holds every move (fail closed). Which kind Grok Build is has not been measured. macOS
also stamps these times lazily: a read or write soon after the last stamp is stamped at the
next `stat`, with that stat's time (measured: 8 of 10 first stats read a 1.15 s old write as
"0 ms ago"). The error only makes an event look newer, so the harness stats its terminal
once per tick (`GROK_ALAB_POLL_MS`) to bound it to one tick; before that fix the first
request after any output was refused as busy, and the move tests failed most runs when run
alone (the full suite passed only because it is slower).

### The journal

The harness writes what it did, one line per event, to
`~/Library/Application Support/grok-alab/journal.log` on macOS (else
`$XDG_STATE_HOME/grok-alab/` or `~/.local/state/grok-alab/`), rotated at 1 MiB: `START`,
`CLAIMED` (the aterm claim), `PASS` (a signal passed on, or `grok` exec'd in the harness's
place and why), `SEEN` (a newer build), `WAIT` (why aterm's read is not yet a safe point),
`MOVE`, `RELAUNCHED`, `FAILED` (a new build that would not start, and the build the session
went back to), `HOLD` (a refused request, and why), `CRASH`, `LOST`/`UNCLAIMED` (the
claim's connection failed, and a retry that did not take), `RELEASED` (the claim given back
because aterm's policy went off), `STOP`/`CONT`, `EXIT`, `REFUSED`, `NOTE`. Each line goes out
as one write on the append-mode file, so the harnesses of every session sharing the journal
never interleave inside a line. A journal it cannot write is skipped; it never writes to the
terminal instead.

### Environment

| Variable | Meaning |
|---|---|
| `GROK_ALAB_PKG_PREFIX` | An absolute package prefix to use instead of atpkg's. |
| `GROK_ALAB_STATE_DIR` | Where the journal goes. |
| `GROK_ALAB_POLL_MS` | How often `current` is read (20 to 60000, default 1000). |
| `GROK_ALAB_QUIET_MS` | The output quiet a move needs (default 20000). |
| `GROK_ALAB_HUMAN_GRACE_MS` | The input quiet a move needs (default 120000). |
| `GROK_ALAB_EXIT_GRACE_MS` | How long the child has to exit for a move (100 to 600000, default 10000). |
| `GROK_ALAB_MIN_RUN_MS` | How long a child must have run for its crash to be relaunched (default 10000). |
| `GROK_ALAB_CRASH_PAUSE_MS` | The pause before the first crash relaunch, doubled for each next one (default 1000). |
| `ATERM_NO_HARNESS` | `1` leaves the harness out of an aterm session: it execs `grok` in its place. |
| `GROK_ALAB_PID` | Set for the child: the harness's pid. |
| `GROK_ALAB_DEPTH` | Set for the child: how many harnesses run above it. A harness that finds 8 or more refuses (exit 126). |

A value that is not a number in range is ignored and journaled.

## What it does not do (yet)

* **It types nothing into any session.** Its scope is `relaunch,upgrade`, which type
  nothing; turn ends, boxes, questions and walls stay aterm's.
* **It writes no session record and announces no act.** Contract 3's `record=` (aterm's
  validated copy, used to clean up after a harness that died) and `harness_act upgrade@v1`
  are not sent yet; if the harness is killed, its `grok` is orphaned as in v0.1.0.
* **It takes no remedy from outside.** aterm's `restart cause=` event (contract 3 item 10) is
  not read.
* **No move under aterm happens without a person's `SIGUSR1` today.** aterm reads Grok's
  screen without authority (its `GrokReader` gives every phase as a non-authoritative idle,
  which `status` reports as **`agent=unknown`** for every Grok session), and `status` carries
  no `composer=`. Both hold the move (Decisions 14, 15), the first one on its own: even once
  `composer=` ships, aterm's read cannot pass until aterm's Grok reader is measured on a
  running Grok Build (aterm's design §4.2 table and §10: Grok's busy row, composer and boxes
  have never been read off a live screen). Until then a newer build waits for `SIGUSR1`, a
  crash relaunch (which takes the newest build) or the next launch. The tests start from the
  screen aterm really sends for Grok, and the automatic move is tested only on a screen
  labelled as a measured reader's.
* It does not read Grok's session files, screen or network, and does not relaunch a child
  the person quit: an exit by code is the session's end.
* It does not install or update `grok`. `grok-alab update` (Grok's self-update), also
  after a prompt (`grok-alab "hi" update`, which clap reads as the `update` subcommand), is
  refused in favour of `aterm pkg update grok`.

## Next milestone

The session record (`record=`) and the `upgrade@v1`/`relaunch@v1` acts, aterm's
`restart cause=` remedy, the harness's own re-exec onto a newer `grok-alab` (aterm's design
§4.5), and, once aterm's Grok reader is measured, moves on aterm's read alone.

## Build and test

Rust here is the Trust toolchain (`rust-toolchain.toml` pins `channel = "trust"`):

```sh
targo --unverified build --release --locked     # target/release/grok-alab
targo --unverified test --locked --workspace
targo tippy --locked --workspace --all-targets -- -D warnings
targo fmt --check
```

The integration tests build a fake `grok` (a shell script) into a scratch package store and
drive the real `grok-alab` binary against it. `tests/aterm.rs` adds a fake aterm control
socket (graph entry, token, `harness policy`, `meta set|unset supervisor`, `status`): under
aterm without contract 3, with contract 2 only, with the policy off, with no policy verb, with
a refused or an unconfirmed claim, with `ATERM_NO_HARNESS=1`, for a subcommand or an uncarried
launch, and with no graph entry, `grok` runs in the harness's place (same pid, shim
environment, the launch's own argv); with contract 3 the claim (exact verb, versions), the
confirmation and only then the child; a move on aterm's read with no signal, held by a draft,
a person typing, an approval box and a missing `composer=`, and the claim re-announced with
the new `vendor=`; `upgrade_ask` and `upgrade = false` holding it and `SIGUSR1` overriding;
a crash relaunched on its session, and a `KILL`, an early crash and an unclaimed crash not;
a crash not relaunched once a fresh read says `[harness] relaunch = false`, or the policy is off
(no event needed), or grok is signed out; the claim given back on a `harness-policy` event
under Autopilot Off and made again once back on; the claim made again within 3 s after aterm
drops every connection, with and without an event stream; the screen aterm ships for Grok
(`agent=unknown`, no `composer=`) never moving it while `SIGUSR1` does; and `run --` handing
every word to `grok`. `tests/harness.rs`, outside aterm, on a pseudo-terminal
where a test needs one: refusal when the vendor is missing or not ready, `--version`, the
home a relative `HOME` stands for taken from the password database (never the relative one),
never exec'ing itself (a store entry that is the harness, one that execs it, one that runs
it as a child, and a newer build or a move target that is the harness), argument
passthrough and exit status, `grok update` refused alone and after a prompt, a signal death
re-raised, `TERM`/`HUP` passed on, `INT`/`QUIT` left to the child, a `HUP` ignored at start
left ignored by the harness and its child, `TSTP`/`CONT` followed, a
move onto a newer build with the resume arguments in the same harness process, a child that
ignores the harness's `SIGTERM` never killed harder, a late exit after it finishing the move
when nobody typed and ending the session with the resume line when someone did, a new build
that refuses to start sending the session back to the old one, no move while grok would start
signed out, and no move without a request, while a process runs under `grok`,
while `grok` draws, while someone types (with a control: the same request moves once the
grace has passed), for an uncarried launch, or with no terminal; and, under aterm, a
signed-out interactive launch asking a fake `aterm` to sign in first (refused with exit 69
and nothing run when it fails or is missing, run once when it signs in, asked for
`dashboard`, `sessions`, `models` and a refused login, never asked for a local subcommand,
help, a one-shot prompt, a signed-in environment or login file, outside aterm or under
`--no-harness`), and, under contract 3, a crash relaunch on a machine signed out mid-session
(by a removed login, or by aterm's refusal marker) asking it first (refused: the crash's own
end and the resume line, no relaunch; signed in: the relaunch follows) and a move that waits
while the machine is signed out without asking aterm.

The fake idles without touching its terminal (a `read -t 1` on a private FIFO) and reads a
typed line only when the test signals it, because an idle `read -t 1` on the terminal moves
the input clock every second (above): with it, the typing test passed with nothing typed.

## Verification

The verified lane, measured 2026-10-25 with `targo` from the session's PATH (targo
0.11.0+g20565c325, Trust 20565c325), each step spelled as `publish/config.sh` spells it but
with that `targo`. `TRUSTD_RUNTIME_DIR` named a private trustd memory domain for step 1,
because another session's trustd (an older toolchain) held the shared socket; targo disclosed
that the run did not share the host memory budget, and nothing else about the run changes.

* `targo trust check --allow-l0-gaps --locked --workspace` (advisory), on v0.3.0's tree with
  the shared journal (aff6a7e): exits 1, `Gate: FAIL [advisory lane, exit 1] (200 proved, 56
  failed, 253 runtime-checked, 275 assumed, 45 mandated, 0 contract-panic, 233 unknown / 1062
  total)`. After Decision 27: exits 1, `Gate: FAIL [advisory lane, exit 1] (191 proved, 1
  failed, 281 runtime-checked, 276 assumed, 47 mandated, 0 contract-panic, 178 unknown / 974
  total)`. 55 of the 56 rows are fixed in the code (Decision 27); the other counts moved with
  the code removed and rewritten, and the failed count is the one that gates. After
  Decision 28 (the password database again for a relative `HOME`, through nix): exits 1,
  `Gate: FAIL [advisory lane, exit 1] (192 proved, 1 failed, 279 runtime-checked, 276
  assumed, 47 mandated, 0 contract-panic, 178 unknown / 973 total)`, the same one failed row.
* **That row was this repository's own, not a Trust defect** (Decision 29; the paragraph that
  stood here blamed `read`'s lowering, and was wrong). `lanes::plan`:
  `call to absent callee grok_alab::lanes::read (body not in the lowered bundle) may panic
  FAILED (level0-refutation-fail-closed-over-runtime_checked)`. `read` was never the site.
  The lane folds every call of a body that may panic into one row named after the first such
  call, and the call that decided this row was `plan`'s own `fresh_id()`: a `pub fn` calling
  a caller's `impl FnOnce() -> Option<String>` claims panic-freedom for every closure, which is
  false (`plan(&[], || panic!())` panics inside `plan`). Trust holds such a call as a named
  instance hypothesis (an `assumed` row) only in a body with no unsafe operation, and
  `plan`'s `vec!["--session-id".to_string(), id]` lowers to a write through the raw pointer of
  a `Box<MaybeUninit<[String; 2]>>` (a union field), so the call stayed a refutation. The
  compiler's own reason for the row names the site ("its co-folded call to `<impl FnOnce() ->
  Option<String> as core::ops::function::FnOnce<()>>::call_once` is an open, uncontracted,
  unresolved GENERIC trait dispatch (the callee this row's text names, `grok_alab::lanes::read`,
  is only the fold's representative and is not itself the refuted site)"), but targo's report
  does not print a failed row's reason. `plan` now takes the id as a value. Measured on
  5329552 with that change, beside 5329552 itself in the same minutes (load about 135 on 18
  cores): the row is `RUNTIME-CHECKED` like the crate's other 168 absent-callee rows, and the
  step exits 3, `Gate: COULD-NOT-RUN [advisory lane, exit 3] (191 proved, 0 failed, 279
  runtime-checked, 276 assumed, 47 mandated, 0 contract-panic, 170 unknown / 963 total)`, where
  5329552 exits 1 with the same counts but `1 failed, 278 runtime-checked`. Exit 3 is two
  obligations a wall clock stopped in both trees, which a failed row outranks:
  `store::decode_vendor_build`'s `s - 1` (`src/store.rs:350`, its hardened panic-boundary row,
  5000 ms, while its overflow row is proved; in every run of 3126670 and 5329552 too) and
  `supervise::run`'s 120 s function budget, spent in VC generation at that load (a run of
  3126670 at lower load finished it).
* `targo --unverified test --locked --workspace`: 58 unit and 46 integration tests pass (32 in
  `tests/harness.rs`, 14 in `tests/aterm.rs`); before Decision 27, 55 and 45; with Decision 28
  and the self-removing unit-test scratch dirs, 63 and 47 (33 in `tests/harness.rs`). The new
  integration test (a HUP ignored at start) fails with both the reset to `SIG_IGN` and
  `Signals::drain`'s filter taken out.
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean (lints are live: the
  same run with `-W clippy::pedantic` warns). `targo fmt --check`: clean.

With the sign-in gate (Decision 26, after its 2026-10-25 review) merged onto v0.3.0, measured
2026-10-25 on this tree with `targo` from the session's PATH: `targo --unverified test --locked
--workspace` passes 54 unit and 45 integration tests (31 in `tests/harness.rs`, 14 in
`tests/aterm.rs`). Each new test fails with its fix taken out: the
refusal marker unread (three tests), the relaunch not asking aterm (the crash test's
control), the relaunch asking outside aterm (v0.3.0's signed-out crash test), and, before the
merge, the local-subcommand list and the gate before each relaunch. `targo tippy --locked
--workspace --all-targets -- -D warnings` and `targo fmt --check` are clean. The verified lane
was not run.

v0.3.0, measured 2026-10-25 on this tree with `targo` from the session's PATH (targo 0.11.0,
Trust 20565c325):

* `targo --unverified test --locked --workspace`: 51 unit and 42 integration tests pass
  (30 in `tests/harness.rs`, 12 in `tests/aterm.rs`), three runs. Nine mutations were
  each caught by a new test: a crash relaunch decided on the claim-time policy, the relaunch
  switch ignored, the policy event ignored (claim never given back), a closed claim connection
  not noticed, the sign-in check skipped for a relaunch and for a move, a late exit never
  finishing the move, typing after the SIGTERM ignored, and a refused start not read.
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean.
* `targo fmt --check`: clean.
* The verified lane was not run for v0.3.0: no proof claim is made for it.

v0.2.0, measured 2026-10-25 on this tree with `targo` from the session's PATH:

* `targo --unverified test --locked --workspace`: 49 unit and 34 integration tests pass
  (27 in `tests/harness.rs`, 7 in `tests/aterm.rs`). Three mutations were each caught by the
  new tests: moving over a draft, claiming without contract 3, and never relaunching a crash.
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean.
* `targo fmt --check`: clean.
* The verified lane was not re-run for v0.2.0: no proof claim is made for it.

v0.1.0, measured 2026-10-24 with the pinned toolchain (`rustup run trust`, Trust
41d799797, targo 0.10.0):

* `targo --unverified test --locked --workspace`: 41 unit and 27 integration tests pass
  (after the 2026-10-24 review fixes; v0.1.0's first commit had 38 and 19).
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean.
* `targo fmt --check`: clean.
* `targo trust check --allow-l0-gaps --locked --workspace` (the verified lane, advisory):
  **exits 1, `Gate: FAIL`**. On the review-fixed tree: 106 obligations proved, 129
  runtime-checked, 45 refuted with no counterexample (failed closed), the rest assumed or
  unknown (v0.1.0's first commit: 104 proved, 44 refuted). 39 of the 45 are
  `hardened_unsafe_operation` rows on the libc calls a process supervisor needs (signal
  handlers, `waitpid`, `kill`, `poll`, `getpwuid_r`, `proc_listchildpids`, ...); five are
  calls through `&dyn Fn` (the environment reader; the fifth is the nesting check's, in
  `lib.rs`) and one is a callee the verifier did not lower. The strict lane also fails
  (106 proved, 45 failed). **No proof claim is made for v0.1.0.**

The release gate (`publish/config.sh`) carries the verified step as it is, so it is red
until those rows are discharged or an owner-signed known-red baseline is committed; nothing
is staged or released past a red gate.

## Rulings

The owner's rulings this repository follows, verbatim:

* "we need every harness (like codex-alab) to be in their own repos with their own versions
  starting from v0.1.0 written in Rust and distributed via atpkg"
* "we do need grok and kimi"
* "I am making repos for claude code harness, muse harness, etc. we also need a staging
  repo for each of these"
* "for the harnesses, there is nothing preventing updating the underlying ai cli underneith
  the harness as the harness is running as a program in aterm"
* "in claude code harness with continue or land on YOU CANNOT BLOCK ON USER FEEDBACK"
* "THE HARNESS MUST DO THIS STOP ASKING THE USER"

And the standing ones: a harness runs the vendor's CLI unmodified as its child and never
ships, forks or patches vendor bytes; its version is its own.

## Decisions

Each choice below was made without asking, taking the fail-closed, reversible option.

1. **The store, never `PATH`, and never itself.** The vendor binary is resolved only from
   atpkg's store (`current`, checked to point at a build directory inside `store/grok/`,
   marked ready, with an executable `bin/grok`). A binary that is the harness itself (same
   path or same file) is refused on the first launch, never moved onto, and never chosen as
   a relaunch target. A store entry that leads back to the harness is refused too: a harness
   whose parent is the harness named by `GROK_ALAB_PID` sits in that harness's vendor slot
   (the entry exec'd it) and exits 126, and a chain of entries that run the harness as a
   child stops at 8 harnesses deep (`GROK_ALAB_DEPTH`). A person nesting a session inside a
   session's tool command is neither (the tool's shell is the parent). Reversible: the
   prefix is overridable (`GROK_ALAB_PKG_PREFIX`), the bound is one constant.
2. **A move needs a witness for the screen as well as the harness's own gates**: aterm's
   read under a contract 3 claim (v0.2.0; it cannot pass on today's aterm, Decision 24), else a
   request (`SIGUSR1`). The harness cannot
   see the screen, and process and terminal quiet are necessary, not sufficient, for "the
   turn is over and nothing is drafted". Ending a session on quiet alone could lose a
   half-typed prompt; without a witness, a missed move costs only waiting for the next
   launch. Rejected: relaunching when the child exits cleanly (it would trap a person's
   own quit).
3. **A fresh interactive launch gets `--session-id <random UUID v4>`.** Grok Build 1.0.70's
   `--session-id` is "Use a specific session UUID for a new conversation", so the harness
   knows the id to resume without reading Grok's files. The relaunch is the launch's own
   flags followed by `--resume <id>` (aterm's resume grammar for Grok:
   `crates/aterm-agent/src/harness/relaunch.rs`, `resume_argv`, and `pool_line`, which puts
   the resume pair after the flags); the first prompt is never typed again.
   `--restore-code` and `--verbatim` are dropped from the relaunch. A launch the harness
   cannot name exactly (`--continue`, `--resume` with a title or no id, `--fork-session`, a
   worktree, an unknown option, a second positional, a non-UTF-8 argument) runs exactly as
   typed and is never moved. Subcommands, `--help`, `--version`, `-p` and the prompt-file
   modes run as typed with nothing added.
4. **The option table is measured, not guessed.** It is Grok Build 1.0.70's own `--help`
   (measured 2026-10-24 from xAI's darwin-arm64 build in a scratch home), plus the aliases
   aterm's vendor row knows. An option outside it makes the launch uncarried, never
   guessed at.
5. **`grok update` is refused** (exit 2) in favour of `aterm pkg update grok`, wherever
   clap would read `update` as the subcommand (after root options or after the prompt;
   clap 4 checks every token that is no option value against the subcommands, and grok's
   one-line usage shows it does not set `args_conflicts_with_subcommands`), and
   `GROK_DISABLE_AUTOUPDATER=1` is always set (aterm's vendor row for `grok`,
   `crates/atpkg/src/vendor_direct/table.rs`): the build that runs is the one atpkg
   verified.
6. **No default posture is added.** aterm's vendor row for `grok` knows its permission
   flags (its yolo markers) for aterm's own launcher; the harness adds no permission flag
   and passes the person's own through. A relaunch keeps every permission flag the launch
   carried.
7. **Only a strictly newer build is moved onto.** A rollback of `current` is not acted on
   mid-session. If the target disappears before the relaunch, the session is resumed on
   the newest build still ready, else on the build it ran.
8. **Never kill harder.** One `SIGTERM` at a safe point; a child that does not exit within
   the grace runs on and the move is abandoned. If it exits later, the move is finished only
   when that exit reads as the SIGTERM's answer and nobody typed since (Decision 22); any other
   late exit may be the person's own quit, so the session ends and the harness prints the
   resume command. Busy means any process under `grok`; a descendant count that cannot be
   read counts as busy.
9. **Thresholds are aterm's own**: 20 s output quiet and 120 s input quiet, the values
   aterm's live upgrades use.
10. **No aterm claim before contract 3** (v0.1.0; v0.2.0 and v0.3.0 keep it as the fail-closed rule).
    The harness claims only from an aterm whose `harness policy` lists `contract=3`, and
    claims only `relaunch,upgrade`, the duties it carries out.
11. **Apache-2.0**, matching aterm (whose layout and resume grammar it follows) and the
    other ALab repositories.
12. **The release gate stays honest.** `publish/config.sh` runs the verified lane as it
    measures today (red) rather than a step that cannot fail; see Verification.
13. **Under aterm: claim first, then spawn, else exec `grok` in place; outside aterm,
    v0.1.0's supervision.** aterm's design §3.2 passes through whenever no socket is
    reachable; outside an aterm tab the harness instead keeps supervising as v0.1.0 did
    (moves on `SIGUSR1` only, no crash relaunch), since nothing there could stand in for it
    and a person who ran `grok-alab` by hand asked for it. In an aterm tab every failure
    before the claim is proven execs `grok`: the tab is then exactly the session that existed
    before harnesses, and aterm's own engine treats it as it always did. Reversible: one
    branch in `run`.
14. **`agent=idle` only.** aterm's design lists `agent=prompt` beside `idle` as a turn that
    ended, but aterm's wire word `prompt` is a permission box (`program_state=blocked`), so it
    holds the move here; recorded for aterm's design too.
15. **A `status` without `composer=` holds every move.** Contract 3 item 9 adds `composer=`;
    until aterm reports it, a draft cannot be ruled out, so aterm's read never moves a session
    and `SIGUSR1` does. The local gates (no process under `grok`, 20 s of terminal output
    quiet) also stay under aterm's read, because aterm's Grok reader is not measured.
16. **`upgrade_ask = true` holds** (the harness has no way to ask yet); the person's
    `SIGUSR1` is the answer.
17. **A crash is a fault or an abort, after at least 10 s of running**, relaunched at most 3
    times with a 1 s pause doubling, only under a proven claim with `[harness] relaunch =
    true`. Grok's exit table is not measured, so no exit code counts as a crash, and `KILL`
    (a person, or aterm's memory ceiling) never does.
18. **`run [--] <grok argv>`** is the command line aterm's twin uses: `run` is never a
    `grok` subcommand, every word after it is grok's (`run -- --version` is grok's
    version), and the v0.1.0 form (`grok-alab <grok argv>`) still works.
19. **A lost claim is made again at once, then every 5 s** (v0.3.0: at once; v0.2.0 waited 5 s),
    on a fresh connection to the socket the graph entry names then; no move or relaunch is
    taken on aterm's word while it is lost. The claim's connection is polled for its close on
    every tick, so an aterm restart or seamless update is noticed within one tick, not at the
    next act.
20. **The policy is followed on its event, and read fresh before every act** (v0.3.0, after
    review: v0.2.0 decided a crash relaunch on the policy its claim was made under). A second
    connection subscribes to `@<sid> events` and re-reads `harness policy` on
    `EVENT <sid> harness-policy` (aterm's design §2, as codex-alab does); where aterm refuses the
    subscription it is re-read every 5 s, and the subscription asked for again. A policy that
    is off for the session gives the claim back (codex-alab gives its claim back under
    Autopilot Off too), and it is made again once the policy is back on. A crash relaunch also
    reads `status` and `harness policy` fresh when the crash happens, so a policy turned off
    with no event in between is still obeyed. Reversible: one branch in `Link::tick`.
21. **A relaunch never starts a grok that would start signed out** (v0.3.0). The harness reads
    credential presence as aterm's connector row for Grok does, never contents; an `api_key` in
    `~/.grok/config.toml` is not read, so such a setup is not relaunched (the session ends with
    the resume line rather than a page opening that nobody asked for). A move is held on the
    same check, so the harness never ends a session for a move it cannot resume.
22. **A late exit after the harness's unanswered SIGTERM finishes the move** when it reads as
    that SIGTERM's answer (exit 0 or `TERM`, within 60 s) and nobody typed since (both the
    terminal's input clock and, under a claim, aterm's `human_ms`; anything unread counts as
    typed). This amends Decision 8: "a late exit cannot be told from the person's own quit" is
    true only when someone typed. Any other exit after it ends the session with the resume
    line. Reversible: `LATE_EXIT_WINDOW` and one branch.
23. **A new build that refuses to start sends the session back** (aterm's design §4.3 step 6):
    within 10 s of a move's relaunch, an exit by a code other than 0, or a fault, resumes the
    session on the build it moved from (if the store still holds it) and marks the new build
    failed for the session, so it is never moved onto again until the next launch.
24. **The tests start from the screen aterm sends for Grok today** (`agent=unknown`, no
    `composer=`), and the automatic move is tested only on a screen named as a measured
    reader's, so no test passes on a screen aterm never sends. This amends Decision 15: the
    first hold is `agent=unknown`, not the missing `composer=`.
25. **v0.3.0.** The relaunch, claim and move behaviour above changed; the patch slot stays 0.
26. **Under aterm, a signed-out start asks aterm to sign in first, and never runs signed
    out** (the sign-in gate, 2026-10-25; aterm `docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md`
    §6, and its harness design's D39). Grok Build signed out opens the sign-in page in the
    desk's browser through macOS's LaunchServices on every launch, whatever its `PATH`, so a
    harness that started it, as a child or in its own place, would open a page per session.
    The harness does not drive the sign-in itself: it delegates to `aterm harness signin --cli
    grok --wait`, which owns the one sandboxed, machine-wide sign-in and its one need. The gate
    runs before the claim and before a pass-through, and inside every relaunch that Decision 21
    would refuse (the machine may have signed out since the start: `grok logout` in another
    tab, or a login grok can no longer renew), so under aterm a crash or a move resumes once the
    machine is signed in instead of ending; a move is still held while signed out (Decision 21),
    so a working session is never ended for a relaunch that would stop at the sign-in. It needs
    no contract: running an aterm verb before `grok` starts is no claim. Fail closed: a sign-in
    that fails, or no `aterm` in an absolute `PATH` entry, exits 69 (`EX_UNAVAILABLE`, as
    atpkg's `grok` twin does) on a first start and ends a relaunch with Decision 21's resume
    line; `grok` never runs. Signed in is one reader for both ([`credentials::signed_in`]): read
    the way the vendor reads it, with the build's `shim-env` applied, never by running `grok`,
    and a login at most as new as its `<file>.refused` marker (aterm writes it when a grok
    started after that login still drew its sign-in screen) is signed out, so a stale
    `auth.json` that grok refuses cannot pass every gate and loop. Gated is every launch but a
    one-shot option (`--help`, `--version`, `-p` and the other one-prompt options) and the
    local subcommands aterm's twin names (`signin::LOCAL_COMMANDS`: local reads and edits, help,
    version, and grok's own `login` and `logout`); `dashboard` opens the interactive screen, so
    it is gated, as is an unknown word. `ATERM_NO_HARNESS` (the twin's `--no-alab-harness`)
    takes the gate away with the rest of the harness. aterm's twin runs the same gate first,
    so under it this one finds the machine signed in. It ships in the v0.3.0 line (no v0.3.0
    was tagged before it). Reversible: the gate is one module, one call in `run` and one in
    `supervise::relaunch`.
27. **The verified lane's refused rows are fixed in the code, never in the gate** (2026-10-25,
    Trust 20565c325; the gate in `publish/config.sh` is unchanged). The environment is a table
    (`env::Env`: the process's own, read where it is needed, or one a test builds), not a
    `&dyn Fn` reader: a call through a trait object names no body, so the lane refused every
    rule that took one (13 rows); the sign-in callback is `&mut impl FnMut` for the same
    reason. Two small dependencies replace libc calls the lane refuses (a zeroed out-parameter,
    a raw deref of the `errno` slot): `errno`, for the signal handler's save and restore of
    `errno`, and `rustix`, for the terminal modes put back after a child (`tcgetattr`,
    `tcsetattr`, `tcgetpgrp`, `getpgrp`); the lane lists both as excluded dependency units its
    result is conditional on, as it lists `libc`. std does the rest: `IsTerminal` and
    `File::metadata` on a duplicate of the descriptor for the terminal's clock;
    `std::env::home_dir` for the account's home when `HOME` is unset or empty (it reads the
    password database exactly then, as the old `getpwuid_r` did; a relative `HOME` now finds
    no home rather than the password database's, fail closed: a regression, which Decision 28
    reverses); and `signal(3)` for the
    handlers (BSD semantics: the same restart and mask as the old `sigaction`). `signal` reads
    a disposition only by replacing it, so a signal ignored at start and kept ignored is
    caught for an instant before it is set back, and `Signals::drain` never reports one; a new
    test starts the harness with HUP ignored and checks neither it nor its child acts on one.
    The claim and aterm's read are reached through one plain borrow of their box (a write
    through the box's own deref is a place the lane does not follow), `run --` drops its
    leading `--` off an iterator rather than `Vec::remove`, and the event stream keeps its
    partial line with `get` rather than `Vec::drain`, so neither has a range that can be out
    of bounds. Reversible: each change is local to its function.
28. **A `HOME` that is not absolute falls back to the password database again** (2026-10-25,
    Trust 20565c325), as it did from v0.1.0 until Decision 27. `std::env::home_dir` reads the
    database only when `HOME` is unset or empty and hands back a relative `HOME` as it is,
    which the resolver refuses, so under Decision 27 a relative `HOME` made every `grok-alab`
    run, `--version` included, exit 2 ("no home directory is known") before it read its
    arguments. That lost a capability rather than closing a hole: a relative `HOME` is still
    never used, and the home comes from the account's own entry. `store::account_home` is
    nix's `User::from_uid(getuid())`, `getpwuid_r(3)` behind a safe call (it sizes its buffer
    from `sysconf` and retries larger, up to 1 MiB, where the old call had a fixed 16 KiB),
    from a third small dependency (`nix`, `default-features = false, features = ["user"]`),
    so no `unsafe` block and no zeroed `passwd` come back into this crate. The verified lane
    lists nix as an excluded dependency unit, as it lists libc, errno and rustix; its advisory
    result goes from 191 proved, 1 failed, 281 runtime-checked, 178 unknown (974 rows) to 192,
    1, 279, 178 (973): `account_home` has one call to an absent callee (`getuid`,
    runtime-checked, where it was `home_dir`) and no closure. A lookup error is dropped by a
    `match` arm on purpose, as v0.1.0 dropped `getpwuid_r`'s; spelled `.ok()` it drew one more
    unknown row (the hardened error-discard check names `Result::ok`, not a match arm), so the
    row it does not draw is no proof of anything. Tests: a relative `HOME` (five spellings),
    an unset and an empty one resolve to the password database's home, read independently
    with `id -P` (`getent passwd` off macOS), and an absolute one is used as it is; these unit
    tests cannot tell `home_dir` from the database while the test's own `HOME` is the
    database's home, so the end-to-end test is the one that fails on Decision 27's tree (exit
    2): `--version`, in a cleared environment, with a relative `HOME` naming a home that holds
    a store says what `HOME` unset says, never that store's build, which an absolute `HOME`
    naming it does say. Reversible: one function and one dependency.
29. **`lanes::plan` takes the fresh session id as a value, not a callback** (2026-10-25). The
    verified lane's last failed row was `plan`'s call to the caller's `impl FnOnce() ->
    Option<String>`: a `pub fn` of this library that runs a caller's closure claims
    panic-freedom for every closure, and a closure that panics refutes it (README,
    "Verification"). `run` now makes the id with `session_id::new_v4()` before the plan is
    known and hands it over; `plan` reads it only where it read the callback, for a fresh
    interactive session, so the plan is the same for every argument list. A launch that is not
    a fresh session therefore also reads 16 bytes of kernel entropy it never uses. Fail closed:
    no id still leaves a fresh launch uncarried. Not taken: a `fn` pointer in place of the
    closure (the same caller's code behind another name), and keeping the callback in a body
    free of `vec!` so Trust would hold the call as an `assumed` instance hypothesis (a premise
    still owed by each caller). Reversible: one signature and its callers.
30. **`--version` names grok's version only from its install record, and otherwise says why**
    (2026-10-27; aterm `docs/DESIGN-harness-repos-2026-10-24.md` §5 and D43). The line is
    `grok-alab <version> (grok <vendor version>)`. The vendor version is the `version` that
    the `current` build's `<build>.vendor` record names, and only when it names that build's
    own id (`store::record_names_build`: the id is decoded to check the record, never to stand
    in for one). With no `current` link the line says `(grok not installed)`; for every other
    reason it says `(grok unknown)`: a store that cannot be resolved, a link that names no
    ready, runnable build, or a record that is missing, garbage or names another version.
    Before, a missing record printed the version the build id encodes and an unusable build
    printed `not usable`, and a store that could not be resolved failed `--version` with
    exit 2 although the harness's own version was known. The claim reads the same field, so
    `vendor=` is left out whenever the line says `unknown` (`aterm::claim_verb`). Journal lines
    still name such a build `build <id>`. `--version` never runs `grok`; the tests check the
    fake's start log. Reversible: one field and the line's three words.
31. **No git tag before a release** (2026-10-27). The version is `[workspace.package] version`
    and the `--version` line; v0.1.0 and v0.2.0 were bumps on the way to the current line and
    were never staged, promoted or tagged. A `v<version>` tag marks a released version only:
    the publication engine's `pub tag` pushes it once `pub promote` has proved the release
    public, and a promote stops at an occupied dev tag that names another commit. Tagging the
    three bump commits by hand would name three releases that never happened and, for the
    current line, block the first real one unless it were cut from exactly the tagged commit.
    So the repository carries no tag until the owner directs the first promote; `pub tag`
    adds it then.
32. **A packed build prints the version its pack signs** (2026-10-27; aterm
    `docs/DESIGN-harness-repos-2026-10-24.md` D46). `atpkg-publish pack` mints
    `MAJOR.MINOR.0+g<sha9>` for the commit it builds and hands it to the build as
    `ALAB_HARNESS_VERSION`; `VERSION` is that value, else the package's `MAJOR.MINOR.0`. The
    `--version` line, the claim's `version=` (`aterm::claim_verb`) and the journal's `START`
    line all read `VERSION`, so a packed binary says what its signed manifest says, and a hand
    build says the bare line, as before. A value that is not this package's line alone or
    followed by `+g` and 9 to 40 lowercase hex digits (`is_pack_version`) fails the build, so a
    wrong one is never printed or claimed. Reversible: unset, it is `CARGO_PKG_VERSION`.
33. **Its first signed pack waits for the owner before any index pins it** (2026-10-31; aterm
    `docs/DESIGN-harness-repos-2026-10-24.md` D50). atpkg build 19, packed from `44531cd` by
    `atpkg-publish pack --upload` and signed by roster machine `incumbent-head`, is staged as
    the dev repository's prerelease `atpkg-grok-alab-19`. No public index pins it: the pin's
    `--upload` mirrors the pack to `alabsystems/grok-alab` first, which does not exist, and
    creating it and the promote that fills it are the owner's. Until then a machine runs it as
    a dev link. Reversible: the prerelease is deleted, or later builds supersede it.
34. **Track all failed target builds in a set and hold automatic re-signalling for an
    unanswered target** (2026-11-03; aterm `docs/DESIGN-harness-repos-2026-10-24.md` §4.3 step
    6). Failed target builds are recorded in a `BTreeSet<u64>` rather than a single slot so an
    earlier failed build is never retried after a second build also fails, and `relaunch_target`
    excludes every failed build at move completion and on crash relaunch. While an unanswered
    move `SIGTERM` still names the same `newer` target build, automatic aterm safe-point polling
    does not re-signal the child.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
