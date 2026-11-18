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

16. **Args helper** (`core-api/src/event_args.rs`). `event_args::Writer` (`empty()`, `put_u8`, `put_u32`,
    `as_slice()`) collects an event's used args bytes in order, big-endian, up to
    `EV_ARGS_MAX_BYTES`. A put that does not fit writes nothing and returns `OutOfRoom`.
    `event_args::Reader` (`new(&[u8])`, `get_u8`, `get_u32`, `finish()`) reads them back; `finish`
    fails with `LeftoverBytes` if bytes remain, and a read past the end is `OutOfBytes`. Add
    `put_u16` and friends when an event needs them. 9 tests.
17. **The core's own events** (`core-types/src/core_event.rs`, event step 3). SUPERSEDED in part by
    item 18: first hand-written, now generated by the derive. The codes, severities, templates and
    args layout below are unchanged.
    `SOURCE` = 0 and `CoreEvent`: `ResetReport { count, previous, consecutive_unclean }` (code
    0x0001, Info, args 4+1+4 bytes), `ResetLogPartialRecord` (0x0002, Warn), `ResetLogCorruptRecord
    { error: reset_log::DecodeError }` (0x0003, Err, args: error kind byte then its value byte),
    `ResetLogMisorderedRecords` (0x0004, Err), `ResetLogCountOverflow` (0x0005, Err). Inherent
    methods, no trait yet: `code()`, `severity()`, `template()` (text with `{field}` placeholders,
    not sent), `encode_args(&mut event_args::Writer)`, `decode(code, args)`. `DecodeError` is strict:
    unknown code, args wrong length (via `event_args::Error`), unknown previous-end code, unknown error
    kind, and a value on an error kind that has none. `PreviousEnd` args code: 0 Unknown, 1
    Commanded, 2 Signal. This is the model for what `#[derive(Event)]` will generate. 17 tests
    including one that sends every event through `Event::new` and the log record codec and
    back.

18. **`#[derive(TypedEvent)]`** (new crate `core-derive`, package `vee-core-derive`, a proc-macro
    crate; dependencies `syn` 2, `quote` 1, `proc-macro2` 1, build time only, never in the flight
    binary). Decided with the user: the event code is the VALUE of the enum variant on an enum
    marked `#[repr(u16)]` (not an attribute argument), so Rust itself rejects duplicate codes
    (E0081); the attribute is `#[event(Info, "text")]` (severity as a bare word: Info, Warn, Err);
    field types are anything implementing `event::Arg`. In `core-api`: `event::TypedEvent` (trait:
    `code`, `severity`, `template`, `encode_args`, `decode`) and the derive of the same name
    (`pub use vee_core_derive::TypedEvent`, so `#[derive(event::TypedEvent)]`), `event::Arg`
    (`WIRE_BYTES`, `put`, `get`; impls for `u8`, `u16`, `u32`, `u64`, `FixedStr<N>`, and
    `reset::PreviousEnd`), `event::DecodeError { UnknownCode(u16), Args(event_args::Error),
    InvalidArg }`. `event_args::Writer`/`Reader` gained `u16`, `u64` and `array` read and write.
    The derive generates the `impl` (one `match` arm or `if` branch per variant) and a `const _`
    that fails to compile if a variant's `Arg` sizes add up to more than `EV_ARGS_MAX_BYTES`. It
    checks: it is an enum, no generics, `#[repr(u16)]`, a value and exactly one `#[event(..)]` on
    every variant, a valid severity, named fields only, and that every `{name}` in the text is a
    field. The generated code names `::vee_core_api`, so a crate using it must depend on
    `vee-core-api`. Inside core-api itself that path would not resolve, so the derive is tested
    from `core-api/tests/typed_event_derive.rs` (11 tests). The compile-fail cases were each tried
    by hand once (10 cases, all give a clear error) but are not automated: that needs `trybuild`,
    a dev dependency we have not added. `CoreEvent` now uses the derive, with `Arg` implemented by
    hand for `reset_log::DecodeError` (in core_event.rs, 2 bytes: kind, value). Its decode errors
    are now the shared `event::DecodeError` (less detail: `InvalidArg`, not
    `UnknownPreviousEnd(9)`). Renamed `args.rs` to `event_args.rs` first (commands have args too).

19. **Events rethought: text only** (DONE; items 11, 13, 14, 16, 17 and 18 describe the old typed
    design: where they conflict with this item, this item is current). Decided with the
    user: an event is a code plus a short text. Typed args are dropped: inside a module a code and
    a sentence is enough, and "if you need more, add another code". Acks and naks are ordinary
    text events (details in the text); the ground reads them. Inside Vee a module that waits on a
    command (an RTS) gets a result for its `seq` from the router, sink-agnostic and non-blocking
    (single thread: submit, keep the `seq`, check on a later cycle). Designed with commands, not
    events. No kind bits in the code, no per-sink ack codes.
    What exists now:
    - `core-api/src/event.rs`: `Severity`, `EV_TEXT_MAX_BYTES` = 50, `Text` (alias for
      `FixedStr<50>`), `Event { source, code, severity, monotonic_us, text }` with all fields
      public (any combination is valid because `Text` always holds valid text; so no `Event::new`,
      no `ArgsTooLong`), and `trait Code { code(), severity() }` with the derive of the same name
      (`#[derive(event::Code)]`).
    - `core-api/src/fixed_str.rs`: `truncated` now marks a cut-off text: it is exactly N bytes and
      the last byte is `TRUNCATED_MARK` (`~`). A text exactly N long is not marked (a text that
      really ends in `~` at exactly N looks cut off; accepted). New `impl fmt::Write`: never
      fails, appends at the end of the text already there, a character that cannot be text (not
      ASCII, or zero) becomes `REPLACEMENT` (`?`), text past N turns the last byte into the mark.
      So `write!(text, ..)` on `FixedStr::EMPTY` builds an event text from `format_args!` with no
      allocation. The `Arg` impl is gone. 27 tests.
    - `core-derive/src/lib.rs`: `#[derive(Code)]` only. The enum must be `#[repr(u16)]` (the
      variant's value is the code; values may be left implicit, counting up from 0, or given
      to pin a code; Rust rejects duplicates, E0081), every variant is a unit
      variant with exactly one `#[severity(Info|Warn|Err)]`. It writes `impl
      ::vee_core_api::event::Code` (`code` (`Self::X as u16`) and `severity`, one `match` with an arm per variant).
      Error cases tried by hand: all give a clear message (a missing value is no longer one). Still not automated (`trybuild`).
    - `core-api/tests/code_derive.rs` (4 tests; replaced `typed_event_derive.rs`).
    - `core-types/src/event_log.rs`: record is 64 bytes: `monotonic_us` 8, `source` 1, `code` 2,
      `severity` 1, `text` 50, `crc` 2. `DecodeError { BadLength, BadCrc, UnknownSeverity(u8),
      BadText(fixed_str::Error) }`. 15 tests.
    - `core-types/src/core_event.rs`: `SOURCE` = 0 and `CoreEvent`, now only codes with severity
      (`ResetReport` 0 Info, `ResetLogPartialRecord` 1 Warn, `ResetLogCorruptRecord` 2 Err,
      `ResetLogMisorderedRecords` 3 Err, `ResetLogCountOverflow` 4 Err; implicit values, so
      new events go at the end). The
      details that were args (count, previous end, unclean count, the decode error) will be written
      into the text where each event is sent.
    - Deleted: `event_args.rs`, `Arg`, `TypedEvent`, `event::DecodeError`, the `Arg` impls for
      `PreviousEnd` and the reset log's `DecodeError`, templates. `core-derive` still uses `syn`
      2, `quote` 1 and `proc-macro2` 1 (build time only). Tests: core 22, core-api 35 + 3, core-types
      29, osal 32. Clippy clean.

20. **Build-time settings** (`core-api/src/config.rs`, design doc section "Build-time settings").
    Each setting is a const defined WHERE IT IS USED, with its default there:
    `config::parse_or(option_env!("VEE_<NAME>"), default)`. The CHECKS on every setting are all in
    `config.rs` (a `const _` assert each, at the end of the file), so they can be audited in one
    place. That means a setting must be defined in core-api, because `config.rs` cannot see the
    other crates. A deployment sets `VEE_<NAME>` in `[env]` in its own `.cargo/config.toml`.
    `parse_or` is a `const fn` that reads a plain whole number; a bad value or a failed check is a
    compile error pointing at the setting. Today: `event::EV_TEXT_MAX_BYTES`
    (`VEE_EV_TEXT_MAX_BYTES`, 50, at least 1) and `reset::RESET_LOG_MAX_RECORDS`
    (`VEE_RESET_LOG_MAX_RECORDS`, 1024, at least 1; moved to core-api `reset.rs` from core so the
    check can see it; the startup code will pass it to `reset_log::start`, nothing uses it yet).
    Precedence checked by hand with a probe of `RECORD_LEN`: real env > `--config` > nearest
    `.cargo/config.toml` > default. Tests that depend on the text size are written in terms of the
    constant, so they pass at any size. To add a setting: define it where used, add its check to
    `config.rs`, add a line to the sample config. `option_env!` finds them all.
    `sample-deployment/.cargo/config.toml` is the example deployment config (the directory is
    otherwise empty; it will hold a sample deployment for a fake mission). Not done: a runtime check
    that the ground tool and the core were built with the same values.

21. **Reset log `report`** (`core/src/reset_log.rs`). `report(&Started, send)` is pure (no I/O):
    it calls `send(core_event::CoreEvent, fmt::Arguments)` once per event: first the reset report,
    then one per problem in the order `start` found them (partial record, then the `read_tail`
    problem). `send` takes `fmt::Arguments` (the result of `format_args!`), so the event service
    writes it into the fixed size `Text` and cuts it off if too long; this function never builds
    a `Text`. The startup code will call `reset_log::report(&started, |code, text|
    ctx.ev.send(code, text))`. The texts: `reset {count}: prev end {previous}, {n} unclean in a
    row` (at most 62 bytes with the largest numbers, so it can be cut off at the default 50; the
    usual case is about 45), `reset log ends in a partial record`, `reset log corrupt record:
    {error}`, `reset log records are out of order`, `reset log boot count is at its maximum`. New
    `Display` impls so the text can embed them: `reset::PreviousEnd` (core-api: `unknown`,
    `commanded`, `signal`) and `reset_log::DecodeError` (core-types: `bad length`, `bad crc`,
    `unknown tag N`, `unknown reason N`, `non-zero padding`). 6 new tests in core (28), 1 in
    core-types (30), 1 in core-api (46 + 4 integration). The tests write the text into a
    `FixedStr<80>` so they do not depend on `EV_TEXT_MAX_BYTES`.

## Next
**The event service**: `ctx.ev.send(code: impl event::Code, text: fmt::Arguments)`. It builds an
`Event` (calls `code()` and `severity()` on the code; fills in `source` and `monotonic_us`; writes
the text into a `Text` with `write!`, which cuts it off and marks it when too long) and queues it.
This needs `ctx`, so decide how much of `ctx` to write first (probably a minimal `Ev` type with a
fixed-size queue, tested on its own). Then the logger (needs the file service). Then the startup
sequence that ties `reset_log::start`, `report` and the event service together.
Possible follow-ups: `trybuild` for the derive's compile-fail cases (adds a dev dependency);
the command derives (`Command`, `CmdSet`) could use the same idea (opcode = enum value).

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
- Size cap is 1024 records (about 7 KB) by default: a build-time
  setting (item 20, now `reset::RESET_LOG_MAX_RECORDS` in core-api), passed to `start` as a
  parameter.
- The reader's rules: any problem (corrupt record, misordered records, count overflow) means the
  history cannot be trusted, so the result is `ResetInfo::first_boot()` plus the problem. A first
  record that is a `Shutdown` is accepted (its `Boot` was before the part read). First boot and
  "no trustworthy history" both give `previous: Unknown`, `consecutive_unclean: 0`.
- Non-fatal problems found at startup (partial record, bad CRC) are returned in `start`'s result,
  not dropped. The caller turns them into events once the event service exists.

## Open questions
- Shutdown reasons beyond `Commanded` and `Signal`.
- Whether a lone `Shutdown` as the first record of a log shorter than the tail window should be
  an error. Today it is accepted, because the reader cannot tell a cut window from a short file.
- Event logger: one file or one per source?
- Time correlation event (monotonic to TAI): when it is emitted, what it carries.
- Whether to add call-site location to events (see item 11).
- How a module waits for a command result without blocking (see item 19). Part of the command design.

## Not started
command routing, software bus, tables, events, file service, `exec!`, modules,
`module-types`, `ground-tool`.
