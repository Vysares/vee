# Latest chat (hand-off between sessions and devices)

This is a short summary of the end of the last session, overwritten each time. It is NOT the source
of truth: `.claude/progress.md` is (status, decisions, next steps) and `docs/design.md` is the design.
Read this after `progress.md` to pick up the thread. Last updated: 2026-11-20 (the user said the session
was ending).

## State when the session ended
- All 308 tests pass, clippy (strict lints) and `cargo fmt --check` are clean.
- The user committed everything up to progress item 59 as `741858c "platform, watchdog servicing, panic
  hook"`. Only a small `progress.md` edit (the open question below) and this file are uncommitted. The user
  commits; do not commit or push unless asked.

## What happened this session (details are in progress.md items 41 to 59)
- Alloc guard, panic hook (one stderr line, a `PANICKING` flag, no latch, no file, no ring), the OSAL
  split into interfaces and `linux/`, the systemd watchdog (a keep-alive on every feed, NO rate limit, NO
  `READY=1`, `StdWatchdog::disabled` as the fallback), `Core<P: Platform>` with the feed at the end of `tick`.
- The software bus was DESIGNED, not written (design.md "Software Bus", progress item 58): explicit APIDs,
  state topics (latest value, `seq`, direct publish) and queued links, explicit read and write lists per
  module with shorthands (`all`), `Topic` trait objects by APID, no generic `S`, no mission wide types crate,
  the recorder as a last-in-order module, table rows by APID and field index (index = wire position, no
  pinned ids), `compare` that never panics.
- Sample deployment: S2a (the start function), S2b (`modules/example`, `Slot` moved to core), S2c (the loop
  at 100 Hz, an overrun skips boundaries, `VEE_SAMPLE_TICKS`, `Clock::sleep_until_us`).
- Overruns showed up in the first runs: the temporary blocking `append` in `Core::tick` (a sync per event).

## Open question waiting for the user
- Should `Clock` be split in two traits (read, and sleep) so that only the executive's loop can wait,
  even inside the core? Today modules cannot reach it (they depend only on `vee-core-api`, which does not
  depend on the OSAL), but `Slot` and `Core` get a `&impl Clock` that has the method (a convention, not a
  compile time rule). About ten lines. The user had not answered.

## Next (see progress.md "Next")
1. Sample deployment S2d: a test that runs the built binary twice in a temp directory.
2. S2e: release profile (`panic = "abort"`, LTO), the guard under a feature, the sample systemd unit and
   logrotate files.
3. The file service (removes the overruns). Then the bus (state topics first), Time and Health, commands.

## Things to remember when continuing
- The user is learning Rust (from C++): explain syntax and generics as they come up. One thing at a time; ask
  before big design forks; after every edit name each file touched and what was done in it (CLAUDE.md rule).
- Do not update this file during a session, only when the user says it is ending (CLAUDE.md rule).
- A test file outside `#[test]` fns needs its own `#![allow(clippy::unwrap_used, reason = ..)]` (and
  `clippy::panic`, `print_stdout`, `arithmetic_side_effects` where used).
- `Core::new` is private on purpose: `Core::start` is the only way to make a core, and callers name the
  platform: `Core::<LinuxPlatform>::start(..)`.
- Run the sample from `sample-deployment/` so its `.cargo/config.toml` settings apply; `VEE_SAMPLE_TICKS=250`
  makes a run end cleanly.
- The user dislikes over-building: the watchdog is optional and low level; no signal handler; no recovery
  event; no hardware watchdog design yet.
