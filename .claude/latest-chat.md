# Latest chat (hand-off between sessions and devices)

This is a short summary of the end of the last session, overwritten each time. It is NOT the source
of truth: `.claude/progress.md` is (status, decisions, next steps) and `docs/design.md` is the design.
Read this after `progress.md` to pick up the thread. Last updated: 2026-11-19.

## State when the session ended
- All 261 tests pass, clippy (strict lints) and `cargo fmt --check` are clean.
- Committed by the user up to `597d1ac "System Log"` (progress items 28 to 36).
- NOT committed: items 37 to 40 and the docs/progress edits: `Core::start`, `Core::stop`, the stop loop
  test, the new core events `ResetLogUnavailable` (8) and `EventLogUnavailable` (9), the design notes on
  the allocation guard and the panic hook. The user commits; do not commit or push unless asked.

## The last exchange (what was said and decided)
1. **Allocation guard.** The user said aborting on an allocation after init makes no sense in flight,
   because the rule exists for safety: the guard is a DEV/SIL/CI check, behind a Cargo feature, off in
   flight builds. Where on, it aborts with a fixed message (a panic could allocate inside the allocator).
   The OSAL provides the `GuardAlloc` type and the deployment's `main` declares `#[global_allocator]`.
   Tests use a per-thread counting mode. Recorded in design.md. NOT written yet.
2. **Panics.** The user asked whether a regular panic could be harmful. Agreed: flight builds keep
   `panic = "abort"` (fail-stop: no zombie threads, no poisoned locks, no `Drop` code writing a false
   `Shutdown`); dev and test builds unwind. No `catch_unwind` per module for now.
3. **Last gasp.** The panic hook runs before the abort, so it can save evidence: the panic location, and a
   dump of an in-memory ring of recent system log lines to a file, which the next boot reports. Events are
   in the ring because `Ev::send` mirrors them at send time. This is step 4 of the system log. NOT written.
4. **Watchdog.** The user noted that systemd restarts the process if it stops being fed. Refinement
   (recorded in design.md, Faults): the first act of the hook is a `compare_exchange` on a static
   `PANICKING` flag (first panicker dumps; losers park and do NOT abort, so the dump is not cut short),
   and the watchdog feed stops while it is set, so a stuck hook on a non-main thread cannot be hidden by a
   ticking main loop. Only in flight builds (`cfg(panic = "abort")`). Send `READY=1`, and keep
   `WatchdogSec` longer than the worst startup.
5. The user asked for this file so sessions can continue across devices.

## Open questions waiting for the user
- Which comes next? My suggested order: (1) the allocation guard (small, closes the start sequence, the
  first `unsafe` in the project), (2) the file service (biggest; removes the blocking `append` from
  `Core::tick`; also the point to introduce the `Platform` bundle trait), (3) system log step 4 (ring,
  panic hook, last gasp file), (4) a small decision: raise the system log's `QUEUE_DEPTH` from 256 to
  1024 to match the event queue? The last message asked: "Should I start with the guard?"

## Things to remember when continuing
- The user is learning Rust (from C++): explain syntax and generics as they come up, with insight
  blocks. One thing at a time; ask before big design forks; after every edit, name each file touched
  and what was done in it (CLAUDE.md rule).
- `Core<F: Files>` stays one type parameter until a SECOND OSAL service arrives (the watchdog or the
  file service). Then bundle them in one `Platform` trait. Do not add it earlier.
- A test file outside `#[test]` fns needs its own `#![allow(clippy::unwrap_used, reason = ..)]`.
- `Core::new` is private on purpose: `Core::start` is the only way to make a core.
