# Vee progress

Handoff notes so work can move between sessions. Update this file at the end of each step.
The design is in `docs/design.md`. If this file and the design disagree, the design doc is the
source of truth for what we want, and this file is the source of truth for what exists.

## Working rules (from CLAUDE.md, restated because they shape every step)
- One small piece at a time. The user must understand every line. Say what derives generate and who calls what.
- Simplicity over cleverness. No magic numbers. Fail loudly, no silent defaults.
- Hand-write before abstracting. A derive waits until a second user needs it.
- Run `cargo test` and `cargo build --workspace` after each step.

## Done
1. **Design doc** (`docs/design.md`): bus (queues), command routing (target + opcode, neutral
   `RawCmd`, decode once), tables (core owns sets, one load buffer and one dump buffer, no lock),
   typed events, reset log, faults (panic = abort, no unwinding), exec, OSAL.
2. **Workspace skeleton**: `core-api`, `core-types`, `core`, `osal`. Packages are named
   `vee-core-api`, `vee-core-types`, `vee-core`, `vee-osal` (a package named `core` would clash
   with Rust's own `core`). Flight crates are `#![no_std]` and opt in to the workspace
   `unsafe_code = "forbid"` lint. `osal` does not opt in. Edition 2024, resolver 3.
3. **CRC** (`core-api/src/crc.rs`): `crc16_ccitt_false(&[u8]) -> u16`, table driven, table built
   by a `const fn`. Tests compare it to a bitwise reference. Residue property: CRC over data plus
   its own big-endian CRC is 0.
4. **Reset log record** (`core-types/src/reset_log.rs`): `ResetLogEntry` (`Boot { count }` |
   `Shutdown { reason }`), `ShutdownReason` (`Commanded`, `Signal`), `DecodeError`, `RECORD_LEN`
   (7). Layout is tag, 4 byte payload zero padded, CRC, big-endian. Hand-written codec.

5. **Reset log tail reader** (`core/src/reset_log.rs`, step 1 of the reset log logic): pure
   `read_tail(&[[u8; RECORD_LEN]]) -> TailReading { info, problem }`, `TAIL_RECORDS` (8),
   `Problem` (`CorruptRecord(DecodeError)`, `MisorderedRecords`, `CountOverflow`). Any problem
   falls back to `ResetInfo::first_boot()` and is reported. Also moved `ShutdownReason` to
   `core-api/src/reset.rs` next to the new `PreviousEnd` and `ResetInfo`, because modules see
   those through `ctx.reset` and only depend on core-api. The log's wire codes for
   `ShutdownReason` stay in core-types as private free functions.

## Next
**Reset log logic in `core`, steps 2 and 3.** Step 1 is done (see Done, item 5). Scoped. Three operations, all blocking and called
straight through the OSAL (not the file service):
- **start** (before any module `init`): read the log tail, rotate to `.1` if over the size cap,
  append `Boot { count + 1 }`, return `ResetInfo` plus the list of problems found.
- **stop** (clean shutdown): append `Shutdown { reason }`.
- **report**: emit an event with the previous end and the new count. Needs the event service,
  so it waits. `start` returns the problems for the caller to turn into events later.

`ResetInfo` = `count`, `previous` (`Clean(ShutdownReason)` or `Unknown`), `consecutive_unclean`.

Steps, each separate and tested before the next:
1. DONE. `ResetInfo` and the tail reader as pure functions. (A partial last record is NOT
   handled by the reader. Step 2 owns it: `as_chunks::<RECORD_LEN>()` on the bytes read gives
   whole records plus the remainder, and a non-empty remainder becomes a reported
   `Problem::PartialRecord` that does not discard the valid records before it.)
2. The OSAL file trait (`read_tail`, `append` with flush, `rotate`; a missing file is a distinct
   result, not an error), an in-memory fake for tests, and `start` and `stop` built on it.
3. The real file implementation in `vee-osal`, tested against a temp directory.

## Decisions to remember
- Only boots are counted. A `Boot` with no `Shutdown` after it means the run ended `Unknown`.
- The log is append-only and independent of systemd. A panic leaves `Unknown`.
- Missing log: count starts at 0. Partial last record: ignored, reported as an event. Bad CRC or
  other unreadable content: error event, count restarts. Never silently treated as valid.
- No modes in Vee. A mission module provides them and persists its own state.
- Core-api owns the CRC so the CCSDS ingest and table files can reuse it.
- Tail read is 8 records (56 bytes). Unclean boots have no `Shutdown` record, so 8 records
  covers up to 8 unclean boots in a row, or 4 boots if every one was clean. `consecutive_unclean`
  is capped at 8.
- Size cap is 1024 records (about 7 KB), a named constant in core for now, passed to `start` as
  a parameter so it can come from elsewhere later. The user wants these kinds of settings to be
  settable by environment variable at COMPILE time (not at startup), for example read with
  `option_env!` when building. Not designed yet.
- The reader's rules: any problem (corrupt record, misordered records, count overflow) means the
  history cannot be trusted, so the result is `ResetInfo::first_boot()` plus the problem. A first
  record that is a `Shutdown` is accepted (its `Boot` was before the part read). First boot and
  "no trustworthy history" both give `previous: Unknown`, `consecutive_unclean: 0`.
- Non-fatal problems found at startup (partial record, bad CRC) are returned in `start`'s result,
  not dropped. The caller turns them into events once the event service exists.

## Open questions
- Shutdown reasons beyond `Commanded` and `Signal`.
- How compile-time environment settings reach the core. Applies to the size cap and likely more.
  Needs a design pass before it spreads.
- Whether a lone `Shutdown` as the first record of a log shorter than the tail window should be
  an error. Today it is accepted, because the reader cannot tell a cut window from a short file.
- The OSAL file trait's exact shape (decided in step 2).

## Not started
`core-derive`, command routing, software bus, tables, events, file service, `exec!`, modules,
`module-types`, `ground-tool`.
