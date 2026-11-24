# Hand-off (end of session, 2026-11-23)

`.claude/progress.md` is the source of truth (item 82 is the newest). This is a short summary.

## Done this session
- The sample's 2 overruns on WSL were explained: the loop woke 2 to 12 ms late (WSL2 scheduler), the work took
  microseconds. Native Linux: none. The overrun warning now says `woke N us late and its work took M us` (item 75).
- Software bus designed with the user and written down in the new `docs/bus.md` (`design.md` points to it; item 76).
  Settled: typed handles (`Module::In<'a>`/`Out<'a>`, `ctx.inputs`/`ctx.out`) for ordinary modules plus a dyn view by
  APID for telemetry, FDIR and the recorder; state topic = one shared slot, `latest()` returns a reference; queued
  topic = a queue per reader, policy per queue, flow control by `writer.space()`; messages get a `LAYOUT` (data:
  name, wire offset, kind) and a hand written `read(bytes, field)`, rows name fields by index; APIDs are unique and
  defined in one place (the deployment), no range check for now; events: one `Ev`, `send` also pushes to a second
  queue for the bus only if a severity filter allows (Off by default, set by a core command), `Ev` has its own step
  at the top of `core.tick`, one tick of delay accepted.
- Bus slice 1 is DONE (items 77 to 82): `core-api/src/bus.rs` (`StateTopic`, `StateWriter`, `StateReader`,
  `Sample`), GATs on `Module`, `Slot::init`/`step` take `time: u64` (one clock read per call), the loop test
  `core/tests/bus_loop.rs`, and the sample's second module `modules/watcher` reading `Example`'s `Progress` topic.
- 391 tests pass, clippy and fmt clean.

## Open
- Next planned: queued topics (queue per reader, policy, `space()`), then `Ev`'s bus queue and flush, the
  `bus::Message` derive with `LAYOUT`, APIDs and the dyn view. Open questions are at the end of `docs/bus.md`
  (how a queued writer reaches its N queues, fan-in fairness, field kinds and `Value`, the bus queue's depth).
- User noted: the reset report text (boot event) could become "From dreamless sleep, Vee wakes." but the count and
  previous-end details live only in that text; it was left unchanged. Idea saved for "the core noop".
- The new sample program test failed once under `--features alloc-guard` (cause unknown, then 20 clean runs); it was
  made less exact (checks invariants, not tick numbers).
- Still open from before: `Core::tick` is not asserted allocation-free.

## Uncommitted
Everything from this session (the user commits): `docs/bus.md`, `core-api/src/bus.rs`, `core/tests/bus_loop.rs`,
`modules/watcher/`, and edits to core-api (`ctx.rs`, `module.rs`, `lib.rs`, `tests/ctx.rs`), `core/src/slot.rs`,
`core/tests/exec_loop.rs`, `modules/example`, the sample deployment, `Cargo.toml`/`Cargo.lock`, `docs/design.md`,
`.claude/progress.md`.
