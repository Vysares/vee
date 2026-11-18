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

6. **OSAL file trait** (reset log step 2, first half): `osal/src/files.rs` has `trait Files`, all
   methods `&self` (real impl is stateless; the fake uses a `Mutex`): `len` -> `Option<u64>`
   (`None` = missing), `read_at` exact read, `read_file` / `write_file` whole file (caller's
   buffer; write is all-or-nothing by contract, real impl must temp-file + rename), `append`
   flushed, `rotate` to `.1`. `FileError` (see item 10 for its current variants). `osal/src/fake_files.rs` has the
   in-memory `FakeFiles` (public, with `put` and `get` helpers for tests). 14 tests. No derives.
   Both logs (reset and event) stay binary, decided because events can be very numerous.

7. **Reset log `start` and `stop`** (`core/src/reset_log.rs`, reset log step 2 complete). Both are
   generic over `F: Files` and return `Result<_, FileError>`. `start(files, path, max_records) ->
   Started { info, problems: [Option<Problem>; 2] }` (slot 0 = `PartialRecord`, slot 1 = the
   `read_tail` problem). `stop(files, path, reason)` appends `Shutdown`. Added
   `Problem::PartialRecord`. 22 tests in core. No derives.
   **Rotation rule:** `start` renames the log to `.1` (replacing any old `.1`) if it is over the
   cap, ends in a partial record (appending would misalign every later record, and `Files` has no
   truncate), or `read_tail` found a problem (else the bad record stays in the 8 record window and
   the count restarts at 1 on every boot until it scrolls out). The count is kept across a
   partial-record rotation, and restarts at 1 after a corrupt-record rotation.
   A missing file skips the read (`read_at` on a missing file is `NotFound`).

8. **Import style conversion** (new rule in CLAUDE.md: import modules, not items, no globs, traits
   only as `use path::Trait as _;`). All crates converted. Test modules use `use super as <name>;`
   (for example `reset_log::start`) in place of `use super::*`. In `core/src/reset_log.rs`,
   `vee_core_types::reset_log` is imported as `log_types` (it clashes with this module's own name
   and with `record` variables), and `start` and `stop` take `fs: &F` so the parameter is not
   confused with the `files` module. In `osal` tests, the fake is called `fake`, not `files`.
   For a generic `F: files::Files`, the trait's methods are callable without importing the trait.
   No behaviour change; same 55 tests pass.

9. **Real `Files`** (`osal/src/std_files.rs`, reset log step 3): `StdFiles`, a unit struct on
   `std::fs`. `impl From<io::Error> for FileError` lets `?` convert std errors (see item 10). `read_at` is seek + `read_exact` (portable, not
   the unix-only `FileExt`). `write_file` writes `<path>.tmp`, `sync_all`, then renames over the
   real file. `append` is create + append + `sync_data`. `rotate` is `fs::rename` to `<path>.1`.
   Not done: syncing the parent directory after a rename, so a power cut right after a rotate or
   write can in theory lose the rename itself (the old-or-new guarantee still holds). 16 tests
   against a temp directory under the OS temp dir (no new dependency). No derives.

10. **`FileError` follows `std::io::ErrorKind`**: variants are `NotFound`, `PermissionDenied`,
   `StorageFull`, `UnexpectedEof` (was `ShortRead`), `Other` (was `Io`), and `TooLarge`, the only
   one with no std equivalent (a file bigger than the caller's buffer). Also implements `Display`
   and `core::error::Error`. `std_files.rs` has `From<io::Error>` so its methods use `?`, which
   replaced the two mapping helper functions. Any std kind not listed becomes `Other`. Add a
   variant (same name as std's) when something needs to tell it apart. `core` only names the type,
   so it did not change. 32 osal tests.

11. **Event envelope designed** (`docs/design.md`, Events and Event Logger sections rewritten).
    Decided: one typed envelope for all events. `Event { source: u8, code: u16, severity,
    monotonic_us: u64, len: u8, args: [u8; EV_ARGS_MAX_BYTES] }`, `EV_ARGS_MAX_BYTES` = 32. Same
    size on the wire and on disk, zero padded. A log record is the envelope plus CRC-16, 47 bytes
    (8 time, 1 source, 2 code, 1 severity, 1 len, 32 args, 2 CRC), big-endian, hand-written codec
    in core-types like the reset log. No run id in records: the reset report event is the first
    event of each run and carries the boot count. Event codes are explicit (`#[event(0x01, ...)]`).
    Severity is kept in the record. `ctx.ev` fills in `source` and `time`, so a module cannot set
    them. Time is `monotonic_us`: microseconds since boot, from the OSAL's monotonic clock, and
    modules get it from `ctx.monotonic()` (renamed from `ctx.now()`). It is NOT MET (restarts each
    boot, no mission epoch). TAI is not in the envelope: a time-correlation event (monotonic plus
    TAI, whenever the offset changes) lets ground convert. Not designed yet. The event log handles
    a partial last record like the reset log (rotate at startup).
    Considered and rejected: cFS-style text in every event (too large, no typed fields for ground);
    a `u32` event code (24 bits of source + code is plenty); a cycle count (monotonic_us already
    orders events). Not in the doc, still pending a yes: call-site location (`#[track_caller]`,
    `Location::caller()`), start with in-memory only, no wire bytes.
12. **`FixedStr<N>`** (`core-api/src/fixed_str.rs`). `struct FixedStr<const N: usize>([u8; N])`,
    ASCII only, exactly N bytes on the wire, zero padded, text ends at the first zero byte, so no
    length field and it can never hold a zero. `EMPTY`, `truncated(&str)` (visible name: cuts off
    at N; rejects any invalid byte even in the cut off part), `from_bytes([u8; N])` (rejects a
    non-ASCII byte and non-zero padding), `to_bytes`, `as_bytes`, and `Display` (prints the
    text, for the ground's templates). `Error { InvalidByte, NonZeroPadding }`. Private field, so
    the invariant always holds. 14 tests. No derives, no dependencies, no unsafe. Considered
    `heapless` and `arrayvec`, but we would write the wire codec ourselves anyway.

13. **Event envelope types** (`core-api/src/event.rs`, event step 1). `Severity { Info, Warn,
    Err }`, `EV_ARGS_MAX_BYTES` (32, with a compile time assert that it fits a `u8`), `Error {
    ArgsTooLong }`, and `Event`. `source`, `code`, `severity` and `monotonic_us` are public.
    `len` and `args` are private so they always agree (len at most the maximum, all bytes after
    len zero), which is why the derived `==` is correct. Only way to make one: `Event::new(source,
    code, severity, monotonic_us, args: &[u8]) -> Result<Event, Error>`, which copies and zero
    pads. `args()` returns the used bytes. Derives on all: `Debug, Clone, Copy, PartialEq, Eq`
    (no `Ord` on `Severity` yet: add it when the downlink filter needs it). No wire codes for
    `Severity` yet: they belong in core-types with the record codec, like `ShutdownReason`'s.
    The record codec will need the full padded args, so add an accessor then. 8 tests.

14. **Event log record codec** (`core-types/src/event_log.rs`, event step 2). Free functions, not
    methods, because `Event` is defined in core-api and Rust does not allow adding methods to
    another crate's type: `event_log::encode(&Event) -> [u8; RECORD_LEN]` and
    `event_log::decode(&[u8]) -> Result<Event, DecodeError>`. `RECORD_LEN` is 47: monotonic_us 8,
    source 1, code 2, severity 1, args_len 1, args 32, crc 2, big-endian, CRC over everything
    before it. Severity wire codes are private (Info 1, Warn 2, Err 3; 0 is never valid, so a
    zero-filled record is rejected). `DecodeError { BadLength, BadCrc, UnknownSeverity(u8),
    ArgsLenTooLong(u8), NonZeroPadding }`. Decode builds the event with `Event::new`, so a decoded
    event is as good as a live one. 13 tests, including every single-bit flip being rejected.
15. **New rule** (CLAUDE.md and the design doc's Faults section): handle errors whenever there
    is a way to, panic only when there is no alternative, and never `unwrap` or `expect` in
    flight code to skip an error case. Example: an event with too-long args is still sent, with
    the args cut off, because the ground log must see it. Consequence still to implement:
    `Event::new` is strict (`ArgsTooLong`, which the record decode also relies on), so the event
    SERVICE must handle that error by sending the event with its args cut to the maximum. Decide
    when the service is written whether the cut event also carries a marker.

Size on 2026-11-17 (cloc, working tree, no target dir): 14 Rust files, 1729 lines of code (about
614 outside test modules, 1115 inside them), 255 comment lines, plus about 409 lines of Markdown.

## Next
**Event envelope in code, step 3: the core's own events, hand-written** in `core-types`. The reset
report is the first (previous end and new count), then one event per `reset_log::Problem`. Each
needs a code, a severity, args encode and decode, and a text template, written by hand as the
model for the future derive. This unblocks `report` in the reset log. The event service queues and
the logger wait for `ctx` and the file service. The derive waits for a second user.

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
- Event logger: one file or one per source?
- Time correlation event (monotonic to TAI): when it is emitted, what it carries.
- Whether to add call-site location to events (see item 11).

## Not started
`core-derive`, command routing, software bus, tables, events, file service, `exec!`, modules,
`module-types`, `ground-tool`.
