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

Under aterm it holds the session under aterm's **harness contract 4** (contract 3 where aterm
serves no 4), through `alab-wire`, the aterm client every ALab harness shares: it claims
`grok-alab@<pid> contract=4 scope=relaunch,upgrade`, names the `grok` it runs to aterm after
every start (`child=<pid>:<start>`), follows aterm's harness policy (giving the claim back under
Autopilot Off), relaunches a `grok` that crashed on the same session on aterm's word and
back-off, makes the restarts aterm asks for (`restart cause=…`, onto another model too), and is
built to take the safe point for a move from aterm's read of the screen. **That read cannot pass
on any aterm that ships today**: aterm reports `agent=unknown` for every Grok session (its Grok
reader is not measured) and no `composer=`, so a move under aterm still waits for `SIGUSR1` or
the next launch (see "What it does not do (yet)"). An aterm that refuses the claim gets plain
`grok`: the harness execs it in its own place and is gone.

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
   build is not moved onto again in the session (aterm's design §4.3 step 6). Under a standing
   claim whose grant no longer takes `upgrade` when `grok` answers the move's `SIGTERM` (a
   person's cap withdrew it meanwhile), the session resumes on the build it ran (Decision 52).
5. **Prints both versions.** `grok-alab --version` (also `-v` and `version`, the spellings
   `grok` answers) prints `grok-alab <version> (grok <vendor version>)` without running `grok`.
   After `run`, every word is grok's: `grok-alab run -- --version` is grok's own version.
6. **Under aterm, claims first, then spawns, else gets out of the way** (aterm's
   `docs/DESIGN-harness-repos-2026-10-24.md` §3.2, §8; the claim sequence is `alab-wire`'s,
   Decision 43). In an aterm tab (`TERM_PROGRAM=aterm`, `ATERM_CHILD=1`, or any
   `ATERM_SESSION_ID`: aterm's own marks, Decision 53) it finds the
   session's control socket (`ATERM_PARENT_SESSION_ID`, the rendezvous directory's
   `graph/<sid>`, the token beside the socket) and reads `harness policy`. It claims at the
   highest contract the policy's `contract=` lists (Decision 44): **contract 4**, `meta set
   supervisor grok-alab@<pid> contract=4 scope=relaunch,upgrade version=<its version>
   vendor=<grok's>`, whose holder aterm proves from its registry row and whose reply states the
   grant; else **contract 3**, the same line at `contract=3`, sent only when `native-act info`
   lists `grok-alab:upgrade` among aterm's roster rows (Decision 37). It confirms the claim with
   `status` (`supervisor=grok-alab@<pid> supervisor_proven=1`, every granted duty in
   `supervisor_scope=`), subscribes to the session's events, and gives back a grant short of
   `relaunch,upgrade`. aterm proves the claim itself: the claim's connection is the harness's
   own, the harness leads the tab's foreground process group, and its executable is a build of
   `grok-alab`. Only then does `grok` start, as its child, and the claim is announced again
   with it (`child=<pid>:<start>` under contract 4), as after every later start. Anything aterm
   refuses (the person's `--no-alab-harness` on the twin, which sets `ATERM_NO_HARNESS=1`; a
   launch the harness cannot carry; no socket; a policy with no `contract=`; a policy off by a
   fact of the window, `unhosted` or `headless`; an `ERR` that does not ask to be retried, such
   as an aterm without the verb; no roster row under contract 3; a refused or unconfirmed claim;
   a refused subscription; a short grant) **execs `grok` in the harness's place**, with the
   launch's own arguments and the build's shim environment: the tab runs `grok` under its own
   name, exactly as if no harness existed. An aterm that cannot take the claim now (Decision 45:
   it asked to be retried, as it does while it shows a dialog, or did not answer a step within
   500 ms once its connection was open, Decision 54; it serves only contract 2; the policy is
   switched off, by Autopilot Off or
   `[harness] enabled = false`) keeps `grok` the harness's child, unclaimed, and the claim is
   made every 5 s until it stands. Opening a connection to aterm may take 2 s, and every
   exchange on one is bounded at 500 ms, the launch's included (Decision 54); the loop wakes on
   the event stream as well as on its tick. The
   claim's connection stays open for the harness's life (the claim is bound to it): when aterm
   drops it (a restart, a seamless update), the claim is made again on a fresh connection every
   5 s, and no move or relaunch is taken on aterm's word meanwhile. The subscription
   (`subscribe @<sid> events`, acked `sub <local> <sid> role=- …`, Decision 38) re-reads
   `harness policy` on `EVENT <local> harness-policy`; a re-read aterm answers `ERR … retry` is
   made again until it answers (Decision 41), and every read of the policy, the safe point's
   included, is followed (Decision 42). When the policy goes off for the session (Autopilot Off,
   `[harness] enabled = false`), the claim is given back (`meta unset supervisor
   holder=grok-alab@<pid>`) and made again once it is back on; a halt (`hold=1`, which the
   policy reads as `why=hold`) keeps the claim and refuses every act on it. An event stream
   that ends while the claim's connection stands is reopened every 5 s, and the policy is read
   at each attempt until it is back (Decision 55).
7. **Relaunches a crashed `grok` on aterm's word and back-off** (Decision 46), under a claim
   that takes `relaunch`. An end aterm counts as a crash (any exit code but 0, 129, 130, 131
   and 143; any signal but `INT`, `QUIT`, `TERM` and `HUP`, `KILL` included) waits aterm's
   ladder (1 s, 10 s, 60 s, then 10 min, forgiven after 10 min of healthy running); then one
   fresh `harness policy` and `status` must still permit it: the claim this harness's, proven,
   with `relaunch`; the policy in effect with `[harness] relaunch = true`; nobody typing within
   `human_grace_s`; no halt and no other driver's hand. aterm busy, or the claim lost, is
   waited for within 45 s; an `INT`, `QUIT`, `TERM` or `HUP` meanwhile ends the session. The
   relaunch resumes the same session, on the newest ready build only while the claim holds
   `upgrade`. Outside aterm, or with no claim, a crash is the session's end. **It makes the
   restarts aterm asks for** (Decision 47): `EVENT restart cause=<cause>` (a person's restart
   through aterm, or aterm's stall remedy) ends `grok` with `SIGTERM` at once and resumes the
   session on the running build, with `SIGKILL` after the exit grace (at most 5 s for a stall);
   `model=<model>` resumes it with `--model=<model>` in place of every model option the launch
   carried, kept for every later relaunch; an account move (`to=`) is refused, since grok runs
   on the machine's one sign-in. Each is made only on a fresh read that permits it and while
   the running build is still ready in the store (Decision 51), waits at most 45 s for an
   aterm that cannot answer, and is passed over once its `grok` is replaced.
   The terminal's modes, taken at start, are put back after every child end while the harness
   owns the terminal.
8. **Never starts a signed-out `grok` itself.** Every relaunch (a crash, a move, a restart, a
   fallback) is
   the harness's own act, and Grok Build started signed out opens xAI's device sign-in in the
   desk's browser. So a relaunch, and the move that would need one, happens only when grok
   would start signed in, read as aterm's row for Grok reads it: a non-empty `GROK_AUTH` or
   `XAI_API_KEY`, else the file `GROK_AUTH_PATH` names, else `${GROK_HOME:-~/.grok}/auth.json`
   (non-empty, and not refused: aterm marks a login it saw grok refuse with `<file>.refused`).
   Otherwise the move is held, and a restart aterm asks for is refused with `grok` running on;
   a relaunch under aterm asks aterm to sign the machine in first (item 9) and follows once it
   has; else the session ends with one line saying why and how to resume once signed in.
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
  waits, with no signal from anyone: one fresh `harness policy` and `status`, judged by
  `alab-wire` as aterm's harness design §4.2 says (`claim::judge_safe_point`), a field aterm
  does not report holding the move: the claim is this harness's, proven, and its
  `supervisor_scope` names `upgrade` (a person's cap that withdraws it hands moves back to
  aterm's own engine, Decision 48); the policy serves the contract and is on, `[harness]
  upgrade = true` and `upgrade_ask = false`; **`agent=idle`** (a turn over, no box, question,
  wall or survey; aterm's `prompt` is a permission box and holds, Decision 14);
  **`composer=empty` or `placeholder`** (a draft holds; an aterm that does not report
  `composer=` holds, Decision 15); **`human_ms`** is `-` or at least `human_grace_s`; `hand=-`,
  `hold=0`, `input=clear` and `need=-`. Each new reason it holds for is journaled once (`WAIT
  not yet a safe point: wait aterm <field>=<value>`, a field aterm does not report as
  `absent`).
* **A request is `SIGUSR1` to the harness**: its sender vouches for the screen, and the
  terminal's input quiet (below) is checked in place of aterm's screen fields. Outside aterm it
  is the only witness; under a claim it is the person's override (also of `upgrade = false` and
  `upgrade_ask = true`) on one fresh read of aterm (`claim::judge_own_move`, Decision 48): the
  claim still this harness's with `upgrade`, no halt, and no other driver's hand. A refusal is
  journaled as `HOLD safe point refused: aterm <field>=<value>`; with the claim lost, the hold
  aterm last stated stands. The harness's pid is in the child's environment as `GROK_ALAB_PID`
  and in the journal's `START` line:

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
again. A child that does not exit within 10 s of the harness's `SIGTERM` for a move is left
running on its build and the move is abandoned. For a move the harness never kills a child
harder, and never ends a busy one (a restart aterm asks for is aterm's remedy, and ends with
`SIGKILL` when it must, Decision 47). A `SIGTERM` cannot be taken back, though: if `grok` honours it after the 10 s, the
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
`CLAIMED` (the aterm claim), `REANNOUNCED` (the claim announced again with a new `grok`),
`NARROWED`/`WIDENED`/`REGRANTED` (the grant aterm shows changed), `PASS` (a signal passed on,
or `grok` exec'd in the harness's place and why), `SEEN` (a newer build), `WAIT` (why aterm's
read is not yet a safe point), `MOVE`, `RELAUNCHED`, `FAILED` (a new build that would not
start, and the build the session went back to), `HOLD` (a refused request, and why), `CRASH`
and `CRASH-WAIT` (a crash relaunch's pause, and aterm not answering it), `RESTART` (a restart
aterm asked for, its `SIGTERM` and any `SIGKILL`), `RESTART-WAIT`, `RESTART-REFUSED`,
`RESTART-PASSED-OVER` and `RESTART-ABANDONED`, `LOST`/`UNCLAIMED` (the claim's connection
failed, and a claim that did not take), `EVENTS-LOST`/`EVENTS-REOPENED` (the event stream),
`POLICY-OWED` (a policy read aterm answered `ERR … retry`, read again every 5 s), `RELEASED`
(the claim given back because aterm's policy went off), `STOP`/`CONT`, `EXIT`,
`REFUSED`, `NOTE`. The aterm lines are `alab-wire`'s link notes, one word each. Each line goes out
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
| `GROK_ALAB_EXIT_GRACE_MS` | How long the child has to exit after the harness's `SIGTERM`: for a move, before it is abandoned; for a restart aterm asked for, before `SIGKILL` (100 to 600000, default 10000). |
| `GROK_ALAB_MIN_RUN_MS` | How soon after a move's relaunch an end reads as the new build refusing to start (0 to 600000, default 10000). |
| `GROK_ALAB_RESTART_WAIT_MS` | How long a restart aterm asked for waits for aterm to answer (100 to 3600000, default 45000). |
| `GROK_ALAB_TEST_AWAY_BUDGET_MS` | Debug (test) builds only: a shorter away budget for a crash relaunch (100 to 45000). A release build never reads it (Decision 56). |
| `GROK_ALAB_TEST_LADDER` | Debug (test) builds only: the crash ladder a session starts with, `<streak>,<since ms>`. A release build never reads it (Decision 56). |
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
* **It moves no session to another account.** aterm's account pool sends `restart cause=…
  to=<label> home=<dir>` for a vendor whose sign-in lives in a home of its own; Grok's lives in
  one login file (`GROK_AUTH_PATH`), so such a restart is refused and `grok` runs on (Decision
  47).
* **No move under aterm happens without a person's `SIGUSR1` today.** aterm reads Grok's
  screen without authority (its `GrokReader` gives every phase as a non-authoritative idle,
  which `status` reports as **`agent=unknown`** for every Grok session), and `status` carries
  no `composer=`. Both hold the move (Decisions 14, 15), the first one on its own: even once
  `composer=` ships, aterm's read cannot pass until aterm's Grok reader is measured on a
  running Grok Build (aterm's design §4.2 table and §10: Grok's busy row, composer and boxes
  have never been read off a live screen). Until then a newer build waits for `SIGUSR1`, a
  crash relaunch (which takes the newest build while the claim holds `upgrade`) or the next
  launch. The tests start from the
  screen aterm really sends for Grok, and the automatic move is tested only on a screen
  labelled as a measured reader's.
* It does not read Grok's session files, screen or network, and does not relaunch a child
  the person quit: an exit by code is the session's end.
* It does not install or update `grok`. `grok-alab update` (Grok's self-update), also
  after a prompt (`grok-alab "hi" update`, which clap reads as the `update` subcommand), is
  refused in favour of `aterm pkg update grok`.

## Next milestone

The session record (`record=`) and the `upgrade@v1`/`relaunch@v1` acts, account moves once
Grok's sign-in has a per-account seam aterm can name, the harness's own re-exec onto a newer
`grok-alab` (aterm's design §4.5), what HX-06's measurement against a contract-4 aterm fixes in
`alab-wire`, taken as alab-harness `v0.1.x` by pinning it again (Decision 57), and, once aterm's
Grok reader is measured, moves on aterm's read alone.

## Build and test

Rust here is the Trust toolchain (`rust-toolchain.toml` pins `channel = "trust"`):

```sh
targo --unverified build --release --locked     # target/release/grok-alab
targo --unverified test --locked --workspace
targo tippy --locked --workspace --all-targets -- -D warnings
targo fmt --check
```

The build needs nothing beside this repository: `alab-wire` is the alab-harness v0.1.0 copy
vendored at `vendor/alab-harness` (its own workspace, excluded from this one) and pinned by its
git tree id in `vendor/alab-harness.pin` (Decision 57). `tests/pin.rs` holds the committed copy
to the pinned tree with no change in the working copy; alab-harness's `tools/check-pin
<checkout>` is the release check, which also holds the pin to alab-harness's tag. To move to a
newer tag, run alab-harness's `tools/pin <checkout> <tag>`, then `git add -f vendor/alab-harness
vendor/alab-harness.pin` and commit. The copy is never edited here.

The integration tests build a fake `grok` (a shell script) into a scratch package store and
drive the real `grok-alab` binary against it. `tests/aterm.rs` runs it against `alab-wire`'s
fake aterm control socket (`alab_wire::fake`, its `fake` feature), from the screen aterm sends
for Grok today. `grok` runs in the harness's place (same pid, shim environment, the launch's
own argv) for a refused or unconfirmed claim, a headless window, an aterm without the policy
verb or with no `contract=`, a refused subscription and a grant without `upgrade` (given back),
and, with no word to aterm, for `ATERM_NO_HARNESS=1`, a subcommand, an uncarried launch and no
graph entry. `grok` stays the child, unclaimed, and is claimed once aterm can take it for a
dialog at the policy or at the proof, a launch read answered late, contract 2 only and
Autopilot Off. The contract-4 claim (exact verbs, no roster read, `child=` once `grok` runs),
and contract 3's behind the roster guard, with no row, no verb or a short scope passing
through. A move on aterm's read with no signal, held by a draft, a person typing, an approval
box and a missing `composer=`, and the claim announced again with the new `vendor=` and
`child=`; `upgrade_ask` and `upgrade = false` holding it and `SIGUSR1` overriding; a halt and a
cap without `upgrade` holding aterm's read and `SIGUSR1` alike; the shipping screen never
moving it while `SIGUSR1` does. A crash relaunched after aterm's 1 s pause and named to aterm,
and a second crash's 10 s pause ended by a `TERM`; a crash not relaunched outside aterm, with a
person at the keys, once a fresh read says `relaunch = false` or the policy is off (no event
needed), or while signed out; a crash relaunch on a machine signed out mid-session (by a
removed login, or by aterm's refusal marker) asking a fake `aterm` to sign in first (refused:
the crash's own end and the resume line; signed in: the relaunch follows). The claim given back
under Autopilot Off and made again, a policy read answered `ERR` owed until it answers, the
policy going off while a newer build waits; the claim made again after aterm drops every
connection, and on a fresh connection after a release aterm refuses. A restart aterm asks for
resuming the session at once, a stall's killing a `grok` deaf to `SIGTERM`, a model restart
resuming on `--model` (kept for a later crash), an account move refused, a restart refused once
its wait runs out and one passed over by a crash relaunch. A move that waits while the machine
is signed out without asking aterm, and `run --` handing every word to `grok`.
`tests/harness.rs`, outside aterm, on a pseudo-terminal
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
`--no-harness`).

The fake idles without touching its terminal (a `read -t 1` on a private FIFO) and reads a
typed line only when the test signals it, because an idle `read -t 1` on the terminal moves
the input clock every second (above): with it, the typing test passed with nothing typed.

The Linux code (`procs.rs`'s `/proc` reader and every `target_os` branch off macOS) was
type-checked and linted for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` on
2026-11-14 from macOS (stable `cargo clippy --locked --workspace --all-targets --target <t> --
-D warnings`), and has never been run.

## Verification

The pinned copy (Decisions 57 and 58), measured 2026-11-14 on this tree with `targo` from the
session's PATH (targo 0.12.0+g29ebb403d), against the vendored alab-harness v0.1.0 (tree
f3b718d), which adds to d64ada4 only the pin tools, alab-harness's README and two of its test
fixtures' placeholder paths:

* `targo --unverified test --locked --workspace`: 76 unit and 70 integration tests pass (35 in
  `tests/harness.rs`, 34 in `tests/aterm.rs`, 1 in `tests/pin.rs`), two runs: the pinned copy
  changed nothing the harness's tests see. `tests/pin.rs` fails with the copy not committed and
  with an ignored stray file under `vendor/alab-harness`.
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean. `targo fmt --check`:
  clean. alab-harness's `tools/check-pin` on this checkout: `carries v0.1.0 exactly`.
* The verified lane was not run: no proof claim is made for this tree.

The review fixes of the port (Decisions 50 to 56), measured 2026-11-14 on this tree against
alab-harness main at d64ada4 (which holds Decision 55's fix, alab-harness Decision 18):

* `targo --unverified test --locked --workspace`: 76 unit and 69 integration tests pass (35 in
  `tests/harness.rs`, 34 in `tests/aterm.rs`), two runs. alab-harness's own suite passes 134
  (`--all-features`).
* Each new test fails with its fix taken out: the late exit with the claim lost moves onto the
  newer build (Decision 50); the restart with the running build gone kills grok and ends the
  session, `EXIT grok could not be started again for aterm's restart (No such file or
  directory …)` (51); the capped move resumes on 1.0.71 (52); the crash whose claim is not
  proven again is still waiting at 30 s on aterm's fixed 45 s budget (56); the second crash
  after a forgiven streak is forgiven again when the relaunch starts no healthy run (56); and
  the claim lingers while the event stream is down with alab-wire's fix taken out (55).
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean. `targo fmt --check`:
  clean.
* The verified lane was not run: no proof claim is made for this tree.

The port onto `alab-wire` (Decisions 43 to 49, aterm tracker HX-10), measured 2026-11-14 on
this tree with `targo` from the session's PATH, against alab-harness main at 97aff86 (which
holds this port's fix, c6db9af):

* `targo --unverified test --locked --workspace`: 75 unit and 62 integration tests pass (35 in
  `tests/harness.rs`, 27 in `tests/aterm.rs`, against `alab-wire`'s fake aterm), two runs.
  alab-harness's own suite passes 131.
* `targo tippy --locked --workspace --all-targets -- -D warnings`: clean. `targo fmt --check`:
  clean.
* The verified lane was not run: no proof claim is made for this tree.

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
    claims only `relaunch,upgrade`, the duties it carries out. (Amended by Decision 44: it
    claims at contract 4 where aterm serves it, still only `relaunch,upgrade`.)
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
    branch in `run`. (Narrowed by Decision 45: a failure aterm says is for now only keeps
    `grok` a supervised, unclaimed child. An aterm tab is named by aterm's own marks, which
    include any `ATERM_SESSION_ID`, Decision 53.)
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
    (a person, or aterm's memory ceiling) never does. (Superseded by Decision 46: a crash is
    aterm's, judged by aterm's rule on aterm's ladder.)
18. **`run [--] <grok argv>`** is the command line aterm's twin uses: `run` is never a
    `grok` subcommand, every word after it is grok's (`run -- --version` is grok's
    version), and the v0.1.0 form (`grok-alab <grok argv>`) still works.
19. **A lost claim is made again at once, then every 5 s** (v0.3.0: at once; v0.2.0 waited 5 s),
    on a fresh connection to the socket the graph entry names then; no move or relaunch is
    taken on aterm's word while it is lost. The claim's connection is polled for its close on
    every tick, so an aterm restart or seamless update is noticed within one tick, not at the
    next act. (Superseded by Decision 49: the event stream's end tells, and the claim is made
    again on `alab-wire`'s 5 s clock, not at once.)
20. **The policy is followed on its event, and read fresh before every act** (v0.3.0, after
    review: v0.2.0 decided a crash relaunch on the policy its claim was made under). A second
    connection subscribes to `@<sid> events` and re-reads `harness policy` on
    `EVENT <sid> harness-policy` (aterm's design §2, as codex-alab does); where aterm refuses the
    subscription it is re-read every 5 s, and the subscription asked for again. A policy that
    is off for the session gives the claim back (codex-alab gives its claim back under
    Autopilot Off too), and it is made again once the policy is back on. A crash relaunch also
    reads `status` and `harness policy` fresh when the crash happens, so a policy turned off
    with no event in between is still obeyed. Reversible: one branch in `Link::tick`. (Kept,
    now as `alab-wire`'s `Link::tick` and `Link::events`, Decision 49; the read on the clock
    while the stream is down was lost in the port and restored, Decision 55.)
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
    line. Reversible: `LATE_EXIT_WINDOW` and one branch. (Lost in the port onto `alab-wire`
    for a claim lost or given back, and restored, Decision 50.)
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
    `vendor=` is left out whenever the line says `unknown` (`alab_wire::claim::claim_verb`). Journal lines
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
    `--version` line, the claim's `version=` (`alab_wire::claim::claim_verb`) and the journal's `START`
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
35. **aterm busy is not aterm gone** (2026-11-13, from kimi-alab 0dd6fe6). aterm answers
    `harness policy` and `status` through its main thread (this decision first said it answers
    `ERR` only once that thread has stalled 5 s, never while it launches; that is wrong, and
    Decision 40 corrects it). A launch claim whose handshake aterm answered and
    whose next read then failed or came late used to exec `grok` in the harness's place, which
    left the session unsupervised for its whole life over one slow answer. Now that is
    `Start::Busy` (`aterm::Unclaimed::Busy`, likewise an `ERR` to the `status` that confirms a
    claim): `grok` runs as the harness's child, the link starts as a lost claim, and the claim
    is made on the 5 s reclaim clock. Nothing moves or relaunches on aterm's word meanwhile. A
    handshake aterm did not answer, and every answer that admits no claim (an `OK` policy that
    does not serve contract 3 or is off, no roster row, a refused or unconfirmed claim; an `ERR`
    to `harness policy` is aterm busy since Decision 40), still passes through: an aterm that said no is believed, the fail-closed
    rule of aterm's design §8.2. A claim made again on a standing connection that fails or reads
    `ERR` now drops that connection (it is out of step after a timeout) and claims again on the
    clock, rather than waiting on the next policy event. Test: a fake aterm that never answers
    `harness policy` at the launch keeps `grok` a child and is claimed once it answers.
    Reversible: one enum arm. (Now `alab-wire`'s failure classes, Decision 45.)
36. **aterm's silence never holds the supervisor's loop** (2026-11-13, claude-code-alab's
    `CONNECT_TIMEOUT`/`REQUEST_TIMEOUT`, kimi-alab's `Conn::bound`). Every socket operation
    waited up to 5 s (`aterm::TIMEOUT`), from the loop that reaps `grok` and passes on its
    signals too, so a stalled aterm stalled the harness's answer to a TERM or a child's stop by
    up to 5 s per exchange. Now the launch's first claim has 2 s per operation
    (`LAUNCH_TIMEOUT`, before `grok` runs) and every later exchange 500 ms (`LOOP_TIMEOUT`): the
    claim's connection is bounded so once the claim stands, and a claim made again or a
    subscription opened from the loop uses it from the start. A slower answer reads as a lost
    claim, made again on the 5 s clock: fail closed (nothing moves without a fresh read), kept
    as kimi-alab's review kept it. Reversible: two constants. (Changed by the port onto
    `alab-wire`, Decision 54: the 2 s now bounds only opening a connection, and the launch's
    exchanges have 500 ms like every other.)
37. **The roster guard, and the scope read back** (2026-11-13, from kimi-alab 14b1c0f). The
    harness claimed without asking whether aterm's roster names it, and accepted any proven
    claim whatever duties it took. aterm stores a contract 3 claim from a holder off its roster
    as a foreign claim taking every duty from its own engine until it is released, and a claim
    granted less than `relaunch,upgrade` leaves duties nobody performs. Now the claim first asks
    the read-only `native-act info` and goes on only when its single `own-image=` list holds the
    exact member `grok-alab:upgrade` (`aterm::rostered`; an `ERR`, `-`, or a repeated list
    claim nothing, not even to find out), and the confirming `status` must read
    `supervisor_scope=` with both `relaunch` and `upgrade`, or the claim is let go. Both checks
    run on every claim, the loop's included. Tests: three rosters and two short scopes pass
    through, the first three with no `meta set` sent. Reversible: one guard, one check. (Now
    contract 3's only, in `alab-wire`; contract 4 has no roster and proves the holder from its
    registry row, Decision 44.)
38. **The subscription is read as real aterm acks it** (2026-11-13, from claude-code-alab).
    aterm acks `subscribe @<sid> events` with `OK subscribe 1` and the row `sub <local> <sid>
    role=- attention=- user_title=-` (aterm's `subscribe.rs` `ack_tail`), and its events name
    the LOCAL id (`EVENT <local> harness-policy`), never the sid. The fake aterm sent the row
    bare, so the tests never saw the real shape. The parser was lenient (any `sub ` row, any
    second word on an event) and so happened to work; now `aterm::sub_row` requires `sub`, a
    numeric local id, the session's sid as the third word and only `key=value` fields after it,
    and `aterm::policy_event` counts only events naming a local id the ack gave. The fake sends
    the real row. Tests: rows with and without fields, a log watch's, another sid's and broken
    ones; events by local id, by another id and by sid. Reversible: two functions. (Now
    `alab-wire`'s event reader, and its fake sends the real row, Decision 43.)
39. **Time stopped is not grace** (2026-11-13, from kimi-alab 0dd6fe6). The grace a child has
    to answer the harness's own SIGTERM for a move ran on through a stop, so a child stopped
    past it (with ^Z, alone or with its whole job) was read as one that ignored the SIGTERM the
    moment it was continued, and the move abandoned. Now the grace starts again whenever the
    child is continued: after the harness continues a child that stopped alone, on the `SIGCONT`
    it passes on, and when `waitpid(WCONTINUED)` reports a continue the harness never saw a stop
    for (a whole-job `SIGSTOP` stops the harness too). The clock is `std::time::Instant`, which
    stops in machine sleep on macOS (`CLOCK_UPTIME_RAW`) and Linux (`CLOCK_MONOTONIC`). The
    harness never sends `SIGKILL`, so this grace is the only one. Test: a child that ignores the
    SIGTERM, stopped 3 s against a 2 s grace both ways, is still within its grace 1 s after it
    is continued, and past it later. Reversible: one helper and one `waitpid` flag.
40. **An `ERR` to the policy or the status is aterm busy, and a release that failed drops its
    connection** (2026-11-13, from an independent check against aterm main and kimi-alab's
    `read_policy`). Decision 35's premise was wrong: aterm answers `harness policy` and `status`
    through its main thread and says `ERR` AT ONCE while it shows a macOS dialog (`ERR aterm is
    showing a dialog; answer it, then retry`, `control_media.rs` `dialog_refusal_for` through
    `main_thread_refusal`) or once a stall is already detected, at a launch as at any other
    time. The launch read an `ERR` policy as a refusal and exec'd `grok` in its place, so one
    dialog up at the launch left the session unsupervised for its whole life. Now an `ERR` (any
    reply that is not an `OK` line, `aterm::answered_ok`) to `harness policy` or to the
    confirming `status` is `Unclaimed::Busy`: `grok` runs as the child, unclaimed, and the claim
    is made every 5 s. `Refused` is kept for an `OK` reply whose words admit no claim or cannot
    be read (no contract 3, the policy off, a status that is not schema 1). An aterm too old to
    know `harness policy` now keeps answering `ERR` while `grok` runs supervised and unclaimed,
    which costs nothing. The roster read and the claim verb were checked and keep their
    refusal: aterm serves `native-act info` off its main thread from a value fixed for its life
    (its `ERR` is an aterm without the verb, or `native entropy unavailable` from a `OnceLock`),
    and serves `meta set supervisor` off its main thread too (its `ERR`s are usage, `unproven`,
    `busy supervisor=<another holder>`, `denied`), so neither `ERR` is a dialog's. Second: a
    claim aterm stored but did not confirm is given back on its connection, and the result
    used to be ignored. A release that timed out leaves its `OK` to arrive later, unread, to be
    read as the answer to the next question on a connection the loop kept (a claim made again
    after the policy came back on), and a release aterm refused may leave the claim standing.
    Now a failed release turns the result into `Unclaimed::Gone`, so the connection is dropped,
    ending the claim bound to it, and the claim is made again on a fresh one (at a launch, that
    is aterm busy). The other releases already dropped theirs (the loop's release when the
    policy goes off loses the claim on any failure). Tests: a fake aterm that answers the
    launch's `harness policy`, then its `status`, with the dialog `ERR` keeps `grok` a child and
    is claimed once it answers, and one without the verb is asked again with no claim sent; a
    claim made again whose release answers 1.5 s late (the loop's bound is 500 ms) drops its
    connection and is claimed again on a fresh one (it failed before the fix). Reversible: one
    predicate and one helper. (Superseded by Decision 45 for the classes: only an `ERR` that
    asks to be retried is aterm busy, as `alab-wire` reads it; the release rule is now
    `alab-wire`'s, alab-harness Decision 16.)
41. **A policy re-read aterm answers `ERR` is owed, not dropped** (2026-11-13, from an
    independent check). While the event subscription stands, the loop reads `harness policy`
    only on an `EVENT <local> harness-policy`, and a read answered other than `OK` was thrown
    away (`follow_policy` returned and the clock was reset). aterm answers that read through its
    main thread, so one dialog up when the policy moved (`ERR aterm is showing a dialog; answer
    it, then retry`) lost the move for good: a claim given back under Autopilot Off was never
    made again once it came back on (no relaunch or move for the session's life), and a held
    claim lingered after the policy went off. The event is spent and another may never come.
    Now such an `ERR` is aterm busy here too (Decision 40): the claim stands as it is, the read
    is owed (`Standing::policy_owed`), and it is made again every 5 s, with the subscription
    standing, until aterm answers `OK`, which is then followed as any other read. A connection
    that fails is still a lost claim. Test: a fake aterm answers the read after each of two
    policy events (off, then on) with the dialog `ERR` once, then `OK`; the claim is given back,
    then made again, on the one subscription (it lingered before the fix). Reversible: one flag.
    (Kept, now `alab-wire`'s, Decision 49.)
42. **Every read of the policy is followed** (2026-11-13, from an independent check). While a
    newer build waits for a safe point under a held claim, the loop reads aterm on every tick
    (`Link::read`), and that read re-reads `harness policy`. It restarted the clock that the
    owed re-read (Decision 41) and the no-subscription 5 s re-read wait on, but never acted on
    what it read and never cleared the owed flag. So with the claim held, a newer build waiting
    (a person typing) and Autopilot set Off while a dialog was up, the event's read met the
    dialog's `ERR`, every tick's read then saw `effective=off` and journaled only `WAIT`, and
    `meta unset supervisor` was never sent for as long as the build waited. Now one rule holds
    for every read: `Link::read_policy` reads the policy (restarting the clock, marking an `ERR`
    owed and clearing it on `OK`) and `Link::follow` acts on it, giving a held claim back when
    the policy is off, leaving it on `ERR` (aterm busy) and losing it on a failed connection.
    The safe-point read goes through both, so a policy it finds off gives the claim back at once
    and its witness is that refusal; the crash relaunch's fresh read gives it back the same way
    (its own release after the read is gone). Test: a
    held claim, a newer build waiting on a person's typing, the policy turned off with the
    event's read (and the tick's that may come first) answered by the dialog `ERR`, then `OK
    effective=off`: the claim is given back within 6 s and nothing moves (it lingered before the
    fix). Reversible: one helper split out of `follow_policy`. (Kept, now `alab-wire`'s,
    Decision 49.)
43. **The aterm client is `alab-wire`** (2026-11-14, aterm tracker HX-10; kimi-alab hx09 and
    agy-alab hx12 made the same move). The wire, the claim sequence, the link held, lost and
    claimed again, the event reader, aterm's safe-point and relaunch judgements and the crash
    ladder are alab-harness's `crates/alab-wire`, shared by every ALab harness, in place of
    the 1411 lines of this repository's own client. `src/aterm.rs` is an adapter: the
    harness's `Config` (`grok-alab`, vendor `grok`, `relaunch,upgrade`, `VERSION`), the
    environment read through `env::Env`, the uptime clock the link's milliseconds run on
    (`CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC` elsewhere, both stop in machine sleep),
    `grok`'s process identity for `child=`, the person's-move judge (Decision 48) and the
    journal: each link note (`aterm claimed …`) becomes one journal event (`CLAIMED …`). What
    is the vendor's stays here: the store, the option table and resume grammar, the sign-in
    gate, the safe point's local gates and the loop. The integration tests run against
    `alab-wire`'s fake aterm (its `fake` feature, a dev-dependency); the fake this repository
    kept is gone. `alab-wire` is the alab-harness v0.1.0 copy vendored at `vendor/alab-harness`
    and pinned by its git tree id, which the public snapshot (`publish/manifest.txt`) ships, so
    it builds on its own (Decision 57). A gap found in porting was fixed on alab-harness main
    with its own test (alab-harness Decision 16: a claim made again on a held link whose
    release aterm refuses drops the connection, which this repository's Decision 40 did and
    `alab-wire` did not). Reversible: the dependency and one adapter module.
44. **Contract 4 first, contract 3 behind it, and a short grant given back** (2026-11-14). The
    claim is made at the highest contract the policy serves: contract 4 sends no roster read
    (aterm proves the holder from its registry row) and its reply states the grant; contract 3
    keeps Decision 37's roster guard and reads its grant from `status`. After every start of
    `grok` the claim is announced again with what it runs (`vendor=`, and under contract 4
    `child=<pid>:<start>` from the kernel's start time), and a contract-4 reply is the grant
    from then on. A grant that changes on aterm's side (a person's cap) is followed both ways
    (`NARROWED`, `WIDENED`). A launch granted less than both duties gives the claim back and
    passes through: a duty taken from aterm's engine and not carried out would be nobody's.
    Reversible: the scopes in `aterm::SCOPES`.
45. **The launch's failure classes are `alab-wire`'s** (2026-11-14). An aterm that cannot take
    the claim now keeps `grok` the harness's child, unclaimed, and claims every 5 s: an `ERR`
    that asks to be retried (a dialog), a step not answered in time after the handshake, a
    policy that serves no contract this harness can claim at (contract 2 only), and a policy
    switched off (`autopilot-off`, `disabled`, `agent-off`). Everything else aterm answered is
    believed and passes through: an `ERR` that does not ask to be retried (an aterm without the
    verb), a policy with no `contract=` (an aterm from before contract 3), a policy off by a
    fact of the window (`unhosted`, `headless`), no roster row, a refused or unproven claim, a
    refused subscription. This supersedes the parts of Decisions 10, 13, 35 and 40 that differ:
    Autopilot Off at the launch and contract 2 now supervise, where they passed through, and an
    `ERR` without `retry` now passes through, where Decision 40 read every `ERR` as busy (its
    one example, a dialog, still reads busy).
46. **A crash is relaunched by aterm's rule, word and back-off** (2026-11-14). Any end
    `alab_wire::relaunch::abnormal_exit` calls abnormal (a non-zero exit but 129, 130, 131 and
    143; a signal but `INT`, `QUIT`, `TERM` and `HUP`, so `KILL` too), under a claim that takes
    `relaunch`, waits the ladder's pause (1 s, 10 s, 60 s, then 10 min; forgiven after 10 min
    of healthy running), and is relaunched only on a fresh `Link::relaunch_word` that permits
    it (the claim this harness's and proven, the policy in effect with `relaunch=true`, nobody
    typing within the grace, no halt, no other driver). aterm busy or the claim lost is asked
    again every second within a 45 s budget of time spent waiting; an `INT`, `QUIT`, `TERM` or
    `HUP` while it waits ends the session with the crash's own status. The relaunch takes the
    newest ready build only while the claim holds `upgrade`, since a move onto it is that
    duty's. This supersedes Decision 17: no limit of 3, no doubling pause, no 10 s minimum run
    (aterm counts an early crash too, and the ladder bounds a loop), and
    `GROK_ALAB_CRASH_PAUSE_MS` is gone. `GROK_ALAB_MIN_RUN_MS` keeps only Decision 23's window.
    An exit by code now counts, which Decision 17 left out because Grok's exit table is not
    measured: aterm's own engine relaunches such an exit, and a claim holder that did not
    would leave the duty it took undone. Reversible: one branch in the loop.
47. **The restarts aterm asks for are made** (2026-11-14). `restart cause=<cause>` on the event
    stream (aterm sends it to a proven `relaunch` holder instead of signalling it) ends `grok`
    with `SIGTERM` at once, with no safe point (the request is aterm's, or a person's through
    aterm), and resumes the same session on the running build; it is no crash, so the ladder is
    not touched. A `grok` that does not end gets `SIGKILL` after the exit grace, or after 5 s
    for a stall (aterm's own stall remedy kills); this narrows Decision 8 to moves. `model=`
    resumes on `--model=<model>` (Grok Build takes `-m`/`--model`), replacing every model
    option the launch carried, and the model stays for later relaunches. An account move
    (`to=`) is refused: grok runs on the machine's one sign-in, and aterm moves accounts by
    `to=` only for a vendor whose sign-in lives in a home of its own (Grok's pool seam is the
    login file `GROK_AUTH_PATH`, read at start). A machine that would start `grok` signed out,
    or an uncarried launch, refuses it with `grok` running on. Each is made on a fresh
    `Link::relaunch_word(asked)`; one aterm cannot answer waits at most `restart_wait` (45 s,
    `GROK_ALAB_RESTART_WAIT_MS`) from its arrival, and a waiting one is passed over when a new
    `grok` starts (it was meant for the one before), when a move starts, while the session
    ends and while a crash's relaunch is pending. Reversible: one branch in the loop.
48. **An upgrade is made only while aterm grants it** (2026-11-14). aterm's safe-point read
    holds without `upgrade` in `supervisor_scope` (`claim::judge_safe_point`), so a person's
    cap that withdraws it hands moves back to aterm's engine. A `SIGUSR1` under a standing
    claim is judged on one fresh read by `claim::judge_own_move` with the person's warrant:
    the claim still this harness's with `upgrade`, no halt (`hold=0`, the policy's `why=` not
    `hold`) and no other driver's hand, while `upgrade = false` and `upgrade_ask = true` stay
    the person's to override. On a lost link the hold aterm last stated stands; with no claim
    the person's word stands on the harness's own gates. The safe point's journal words are
    `alab-wire`'s (`wait aterm agent=unknown`), and this repository's own screen judge is gone.
    Reversible: one adapter function.
49. **What else `alab-wire` now does** (2026-11-14). A lost claim is made again on its 5 s
    clock, not at once (Decision 19); the event stream's end is noted (`EVENTS-LOST`) and the
    stream reopened on the same clock; the policy owed after an `ERR … retry` and every read
    followed (Decisions 41, 42) are its rules; every exchange is bounded by its 500 ms and the
    launch's connections by its 2 s (Decision 36); and the loop waits on its event descriptor
    beside the signal pipe, so a restart or a policy event is answered without waiting out a
    tick.

50. **A late exit with aterm's input clock unread ends the session** (2026-11-14, from the
    review of the port). Decision 22 counts anything unread as typed, but the port judged a
    late exit after the move's unanswered `SIGTERM` on the terminal's clock alone whenever no
    claim stood (`!Link::judges`), so a claim lost (an aterm restart) or given back (Autopilot
    Off) between the `SIGTERM` and grok's late exit 0 let the move finish onto the newer build
    when the person may have quit. Now only a session with no link to aterm at all (outside
    aterm, `Link::is_absent`) is judged on the terminal's clock alone; under aterm a fresh read
    of the standing claim must show nobody within the window (`human_ms`), and a lost, given
    back or unanswered read counts as typed. The journal says which clock: `a person may have
    typed since: aterm's input clock was not read (…)`. Test: the late exit with the claim
    lost, and given back, ends the session with the resume line (both moved before the fix);
    with the claim standing it still finishes the move. Reversible: one guard.
51. **A restart aterm asks for is refused when the running build is gone** (2026-11-14, from the
    review). A restart ends grok and resumes the session on the build it ran, but nothing
    checked that build was still in the store before the `SIGTERM`, so after atpkg removed it
    (a newer install), aterm's restart killed a healthy session and its relaunch failed (exit
    127). Now it is refused before any signal unless the running build is still ready with its
    executable (`startable`), as a machine signed out refuses it, and grok runs on (a stall's
    remedy asks again while the stall stands). Moving the restart onto the newer build instead was
    rejected: that is the `upgrade` duty's move, made at a safe point, not a restart's. A build
    removed between the check and grok's exit still ends the session with the resume line.
    Test: the running build removed, `restart cause=asked` is refused and grok gets no
    `SIGTERM` (it was killed and the harness exited 127 before the fix). Reversible: one check.
52. **A move finishes onto the newer build only while the grant allows it** (2026-11-14, from
    the review). The move's relaunch took the newest build whenever grok answered its
    `SIGTERM`, though a person's cap may have withdrawn `upgrade` meanwhile (`NARROWED
    scope=relaunch`), while a crash relaunch already took the newest build only under
    `upgrade`. Now a standing claim whose grant lacks `upgrade` when grok ends resumes the
    session on the build it ran (`HOLD aterm no longer grants upgrade …`). Without a standing
    claim the move finishes as before: outside aterm, or with the claim given back, a move is
    the person's own (Decision 48), and the `SIGTERM` was sent on a word that stood then. Test:
    the cap lands while grok holds the move's `SIGTERM`, and grok's exit resumes on the build it
    ran (it moved before the fix). Reversible: one condition.
53. **An aterm tab is named by aterm's own marks** (2026-11-14, from the review). The port reads
    "inside aterm" as `alab-wire` does: `TERM_PROGRAM=aterm`, `ATERM_CHILD=1`, or any
    `ATERM_SESSION_ID`, the marks aterm's agent brief names and kimi-alab and agy-alab read
    too; before, `ATERM_SESSION_ID` alone did not count. A launch with only that mark is in an
    aterm tab, so when no session can be found from it (no `ATERM_PARENT_SESSION_ID`) grok runs
    in the harness's place as for any tab without a socket (Decision 13), where before the
    harness supervised it as outside aterm. Kept: aterm's own engine then sees grok as the
    tab's program, which is the fail-closed answer inside aterm, and one rule for every
    harness. Test: `ATERM_SESSION_ID` alone passes through; no mark at all supervises.
54. **The launch's exchanges have 500 ms** (2026-11-14, from the review). Decision 36 gave the
    launch's claim 2 s per exchange; `alab-wire` gives 2 s to open and authenticate each
    connection and 500 ms to every exchange on it, the launch's included, so an aterm answering
    a launch step in 0.5 to 2 s, which was claimed at the launch, now leaves grok supervised and
    unclaimed, claimed again every 5 s with the same 500 ms. Kept: it is `alab-wire`'s rule
    for every harness, measured against a real aterm by claude-code-alab (aterm answers in
    milliseconds, and says `ERR … retry` at once while it cannot read its state), so an answer
    that slow is a stalled main thread, and the session is supervised meanwhile either way.
    Widening every harness's launch exchanges was rejected without a measured aterm that needs
    it. This corrects item 6 and Decision 36, which said 2 s.
55. **The policy is read on the clock while the event stream is down** (2026-11-14, from the
    review; alab-harness Decision 18). When the event stream ended and could not be reopened
    while the claim's connection stood, `alab-wire` read the policy once and then not again
    until the stream returned, so Autopilot Off left the claim standing; Decision 20 had read it
    every 5 s. Now each failed reopen also reads it, and a policy read aterm owes is journaled
    once (`POLICY-OWED`), as Decision 41's note was. Test: the stream ended with its
    resubscription refused, Autopilot Off gives the claim back within one attempt and back on
    claims again, the claim's connection never lost; the stream reopened, a restart on it is
    made.
56. **A test build may shorten the away budget and seed the crash ladder** (2026-11-14, from the
    review, alab-harness Decision 17). The crash's 45 s wait and the ladder's 60 s and 10 min
    steps and 10 min forgiveness were tested only in `alab-wire`'s unit tests. A debug build
    reads `GROK_ALAB_TEST_AWAY_BUDGET_MS` (shorter only) and `GROK_ALAB_TEST_LADDER=<streak>,<since
    ms>` (`Ladder::carried`), so the end-to-end tests show a crash while the claim is lost
    relaunched once it is made again, one passed on when the budget runs out
    (`claim-not-proven-again`), each later step's pause, and a healthy run forgiving the streak
    and the next relaunch starting a new one. A release build reads neither (`cfg!(debug_assertions)`)
    and answers aterm with aterm's budget and ladder. Reversible: two knobs.
57. **`alab-wire` is a vendored copy of alab-harness v0.1.0, pinned by its tree id**
    (2026-11-14, aterm tracker HX-08; alab-harness Decision 19). Decision 43 left `alab-wire` a
    path dependency on a sibling alab-harness checkout until alab-harness had a remote and its
    v0.1.0 tag, then a git tag dependency. Neither can ship: aterm's `deny.toml` refuses git
    sources, and a sibling path kept this branch from main and from a package, since a public
    clone did not build. alab-harness tagged v0.1.0 before HX-06 lands (its Decision 19), and
    its `tools/pin` copies that tag's tree, byte for byte, to `vendor/alab-harness` and the
    tree's git id to `vendor/alab-harness.pin`. Both `alab-wire` lines in `Cargo.toml` name
    `vendor/alab-harness/crates/alab-wire`, and the workspace excludes `vendor`, the copy's own
    workspace. A git tree id is equal exactly when every path, mode and byte is, so one
    comparison holds the whole copy: `tests/pin.rs` holds the committed copy to the pin and
    refuses any change in the working copy, an ignored stray file included, and alab-harness's
    `tools/check-pin` is the release check, which also holds the pin to the tag.
    `publish/manifest.txt` ships `vendor`, so the public snapshot builds alone. The copy is
    never edited here: a defect in it is fixed in alab-harness and taken as a new `v0.1.x` by
    running `tools/pin` again and committing, as HX-06's measurement will be. Reversible: the
    copy, its pin, two dependency lines and one test.
58. **`alab-harness.toml` restates aterm's row for this harness** (2026-11-14, aterm tracker
    HX-10, whose harness ships `alab-harness.toml`, and HX-37, which shrinks the default strip
    list; the seed row is `SEEDS` in aterm's `crates/atpkg/src/harness_record.rs`, which HX-05
    adds; kimi-alab carries the same record). `atpkg-publish pack`
    copies its `[harness]` table into the package's signed manifest, and aterm makes a registry
    row from it only when that manifest verifies. `id`, `tool`, `leads` and `name`
    (`grok-alab`, `grok-alab`, `grok`, `Grok Build`) restate aterm's compiled seed row, so a
    build carrying the record changes nothing when it replaces that row; `env_strip` (`GROK_`,
    `XAI_`) restates the prefixes aterm's default strip list holds for this vendor, so that
    default can shrink without the vendor's variables reaching shells; `contracts = [3, 4]`
    because `alab-wire` claims at 4 where aterm serves it, else at 3 (Decision 44). The public
    snapshot ships it. Reversible: one file and one manifest line.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
