# Latest chat (hand-off between sessions and devices)

This is a short summary of the end of the last session, overwritten each time. It is NOT the source
of truth: `.claude/progress.md` is (status, decisions, next steps) and `docs/design.md` is the design.
Read this after `progress.md` to pick up the thread. Last updated: 2026-11-19.

## State when the session ended
- All 270 tests pass, clippy (strict lints) and `cargo fmt --check` are clean.
- Everything up to progress item 40 is committed. NOT committed: item 41 (below) and the docs edits.
  The user commits; do not commit or push unless asked.

## The last exchange (what was said and decided)
1. The user said to go ahead with the allocation guard and that the system log's `QUEUE_DEPTH` can be
   raised: done, 256 to 1024 (`osal/src/std_syslog.rs`, design.md).
2. **Allocation guard written** (`osal/src/std_alloc_guard.rs`): `GuardAlloc` (forwards to `System`,
   counts per thread, aborts with a fixed message once `arm()` was called), `arm()`,
   `allocations_on_this_thread()`. I dropped the Cargo feature from the earlier plan (the deployment's
   `#[global_allocator]` declaration is the switch) and said so; the user did not object yet, so ask if
   unsure. Tests: counting, zero allocation in syslog `emit` (both modes) and `drain`, and an abort test
   that re-runs the test binary as a child (`osal/tests/alloc_guard_abort.rs`).
3. `Core::tick` is NOT asserted allocation free: `FakeFiles::append` allocates; do it after the file
   service replaces the blocking `append`. `arm()` has no caller yet (the exec's start function).

## Open questions waiting for the user
- Which comes next? My suggestion: the file service (biggest; also where the `Platform` bundle trait
  is introduced), then system log step 4 (panic hook and last gasp file; the ring was dropped).

## Things to remember when continuing
- The user is learning Rust (from C++): explain syntax and generics as they come up, with insight
  blocks. One thing at a time; ask before big design forks; after every edit, name each file touched
  and what was done in it (CLAUDE.md rule).
- `Core<F: Files>` stays one type parameter until a SECOND OSAL service arrives (the watchdog or the
  file service). Then bundle them in one `Platform` trait. Do not add it earlier.
- A test file outside `#[test]` fns needs its own `#![allow(clippy::unwrap_used, reason = ..)]`.
- `Core::new` is private on purpose: `Core::start` is the only way to make a core.
