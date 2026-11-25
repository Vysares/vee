# Hand-off (end of session, 2026-11-24)

`.claude/progress.md` is the source of truth (item 97 is the newest; "WHERE WE LEFT OFF" has the state). This is a
short summary.

## Done this session
- Command router finished for the core (items 92 to 96): `CoreCommands` (`core/src/commands.rs`), command sinks
  (`cmd::Sink`, one queue each, the module gets a `SinkHandle` through its `Out`), `Core::route_commands`, the core
  draining its own commands in `Core::tick`, `ctx.drain_cmds` so a module writes only the match and the outcome ack
  ("succeeded/failed/pending", the module's own source, never throttled) is baked in, a core `Noop` command,
  `CmdSet::opcode()`, and the sample deployment's `pinger` module sending that `Noop` once a second (both acks
  come back; also checked under `--features alloc-guard`).
- Testing pass (item 97): `trybuild` (43 must-not-compile programs, 10 on-the-limit programs that must compile),
  `LogWriter` tests, boundary tests across the crates, bus loop tests. Two real bugs found and fixed: `pop_encoded`
  lost a message when the caller's buffer was one byte short, and a log cap of 0 failed on a missing log.
- `cargo-llvm-cov` installed (with the user's OK). Coverage 97.6% regions, 97.4% lines. 622 tests, clippy and fmt clean.
- Size: 4,090 lines of Rust excluding tests (61 files), 2,171 comment lines.

## Open
- TECH DEBT FIRST: `too_many_arguments` is allowed (with `TODO(revisit)`) on `Ctx::new` and `Slot::step`; bundle
  `inputs`, `out`, `cmds` into one struct before `Ctx` grows again.
- The user wants a better way of assigning target IDs (duplicate ids: first wins, untested). Worked out later.
- Not written: the link module, the result for the sender's `seq`, a sink registry, `flush_bus` in `Core::tick`,
  tables, `exec!`, the recorder, telemetry, FDIR. Remaining coverage gaps are listed in item 96/97 (mostly "cannot
  happen" branches, the writer thread failing to spawn, the alloc guard's abort path, the sample's startup failures).

## Uncommitted
Everything from this session (the user commits): new `core/tests/{cmd_loop,log_writer,bus_flow_loop}.rs`,
`core-api/tests/compile_fail.rs` and `core-api/tests/ui/`, `sample-deployment/src/pinger.rs`, and edits across
`core-api`, `core`, `core-types`, `core-derive`, `osal`, the sample deployment, `docs/design.md`, `docs/bus.md`,
`Cargo.toml`/`Cargo.lock` and `.claude/progress.md`.
