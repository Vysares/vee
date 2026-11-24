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
    Declared where used with the macro `config::setting!` (a `macro_rules!`, `#[macro_export]`
    and re-exported in `config` so it is called as `config::setting!`; it uses `$crate::`, so it
    works from any crate, including modules): `pub NAME: ty = default, min..=max;` plus doc
    comments. It writes `const NAME: ty`, reads `option_env!("VEE_NAME")` (the env var name is
    made from the identifier, so it cannot drift), and checks the range (the default too) in the
    same `const`; also that the largest allowed value fits the type. `min` and `max` must be
    literals. `parse_or` (a `const fn`, plain whole numbers only) does the reading. All errors are
    compile errors that name the setting and point at its declaration. No central file lists
    settings: search for `setting!` (this replaced the checks block I first put in `config.rs`,
    which could not see other crates). A deployment sets `VEE_<NAME>` in `[env]` in its own
    `.cargo/config.toml`. Today: `event::EV_TEXT_MAX_BYTES` (50, 1..=4096) and
    `reset_log::RESET_LOG_MAX_RECORDS` (1024, 1..=1000000, in core `reset_log.rs`; the startup code will pass it to
    `reset_log::start`, nothing uses it yet). The RANGES are my guesses: the user has not set them.
    Same-named settings in two crates would share one env var (modules should prefix names).
    Precedence checked by hand: real env > `--config` > nearest `.cargo/config.toml` > default, and
    an override reaches a setting declared in another crate. Compile-fail cases tried by hand
    (default out of range, range wider than the type, env out of range, env not a number, stray
    comma). `sample-deployment/.cargo/config.toml` is the example deployment config (the directory
    is otherwise empty; it will hold a sample deployment for a fake mission). Not done: a runtime
    check that the ground tool and the core were built with the same values.

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

22. **Generic queue `Queue<T, N>`** (`core-api/src/queue.rs`, new file; in core-api so modules
    can use it too). A fixed-size FIFO ring buffer on the main thread, no allocation, `T` need not
    be `Copy` (`[const { None }; N]`). `new()` (a `const fn`), `push(item) -> Result<(), T>` (when
    full the item is GIVEN BACK in the `Err`, so each queue picks its own policy: ignore it =
    drop newest, `pop` then `push` = drop oldest, or turn it into a nak), `pop()` (oldest first),
    `len()`, `is_empty()`, `Default`. No panic paths (`get_mut`, invariants in comments); `N` of 0
    holds nothing. Not thread safe: cross-thread queues (the file service) are the OSAL's. Written
    by hand, not `heapless`: about 40 lines, no dependency. 10 tests (core-api: 58).
23. **The event service `Ev`** (`core-api/src/ev.rs`; it started in core and was MOVED to core-api
    and made NON-GENERIC with the user: there is only one event queue, so its depth is the setting
    `ev::EV_QUEUE_DEPTH` (`VEE_EV_QUEUE_DEPTH`, 1024 since 2026-11-19, was 32; `1..=65535`), and `Ctx`, which modules see and
    which therefore lives in core-api, can hold a `&mut Ev` directly: NO `EventSink` trait (the user
    chose the no-trait route; an `EventSink` would only pay off with a second implementation).
    `Ev` holds a `Queue<Event, EV_QUEUE_DEPTH>` and a `dropped` count. `Ev::new()` (a `const fn`),
    `send(source, monotonic_us, code: impl event::Code, text: fmt::Arguments)`, `pop()` (oldest
    first), `len()`, `is_empty()`, `take_dropped() -> u32` (returns the count of events dropped
    because the queue was full and resets it to 0; the logger will send it on as an event).
    `send` never fails: the text is written into `event::Text` with `write_fmt` (too long: cut off
    and marked; a value whose `Display` fails: the text so far is kept, the error is ignored on
    purpose), code and severity come from `Code`, and a full queue DROPS THE NEW event (keeps the
    oldest, since the first events of a problem matter most) and counts it. No I/O, no clock: the
    caller passes `source` and the time. Tests: `core-api/tests/ev.rs` (new file; 10 tests, written
    in terms of the depth setting, with a derived sample `Code`), and one in `core/src/reset_log.rs`
    that sends `report` through `Ev` (it needs core-types, which core-api cannot see). core: 41.

24. **Event log, file side** (`core/src/event_log.rs`, new file). `start(fs, path, max_records) ->
    Result<Started { partial_record }, FileError>`: if the file has at least `max_records` whole
    records, or its length is not a multiple of the record size, it is rotated to `.1` (like
    `reset_log::start`); nothing is read and nothing is appended (no boot record: the reset report
    event is the first event). `report(&Started, send)` calls `send` with
    `CoreEvent::EventLogPartialRecord` (Warn, code 5, new, at the end of the enum) and the text
    `event log ends in a partial record`. `append(fs, path, &Event)` encodes one record with
    `event_log::encode` and appends it. All blocking through `Files`: this is the code the file
    service's THREAD will run. The main thread must never call `append` in the tick loop: it will
    encode events and hand them to the file service (possibly a thread of its own for the log) over
    an OSAL cross-thread queue. Not written: the thread, the queue, the main-thread half (pop from
    `Ev`, encode, push, `take_dropped` as an event), batching several records into one append (a
    file sync per record is slow), and what to do when `start` fails with a `FileError`.
    New setting `event_log::EVENT_LOG_MAX_RECORDS` (`VEE_EVENT_LOG_MAX_RECORDS`, default 10000,
    range 1..=1000000; the user expects to raise the default 10x later). The setting identifier is
    the full env name without `VEE_`, so the names stutter (`event_log::EVENT_LOG_MAX_RECORDS`).
    Also moved `RESET_LOG_MAX_RECORDS` from core-api `reset.rs` to core `reset_log.rs` (where it
    is used). Open question still open: one log file or one per source (kept one). 12 tests. core:
    50.

25. **Event throttling** (the pieces that do not need `ctx`). Decided with the user: limit per
    TICK (one executive cycle / minor frame; commands run inside ticks) per source per code; a
    separate lookup table was rejected (it can fill up), so there is ONE COUNTER PER DEFINED EVENT,
    found by the event's position. Settings: `throttle::EV_MAX_PER_TICK` (`VEE_EV_MAX_PER_TICK`, 5,
    `1..=255`, `u8`) and `throttle::EV_MAX_CODES` (`VEE_EV_MAX_CODES`, 256, `1..=65535`); both in
    the sample config.
    - `event::Code` gained `index(&self) -> usize` (the variant's position in the order written, NOT
      the code) and `from_index(usize) -> Option<Self>`. The derive writes both (one `match` arm per
      variant) and a `const _` assert that the enum has at most `EV_MAX_CODES` variants (compile
      error naming the enum, saying to raise `VEE_EV_MAX_CODES`). History: first a const-generic
      `Throttle<C, N>` with a derived alias; then NON-GENERIC with the user: every source's
      throttle is the same size, so `Ctx` carries no `N` (the slice idea was dropped), and the
      counters are `u8` (255 stops "far over the limit"; a `u16` gains nothing).
    - `core-api/src/throttle.rs` (new file): `Throttle { sent: [u8; EV_MAX_CODES] }` (256 bytes per
      source). `new()` (a `const fn`), `throttle<C: Code>(&mut self, &C) -> Option<u8>` (counts a
      send; `None` = send it; `Some(over_by)` = suppress, `over_by` is 1 for the first suppressed
      and stops at 255 minus the limit), `new_tick<C: Code>(&mut self, report: FnMut(C, u8))` (calls
      `report(event, suppressed_count)` for each event of enum `C` suppressed in the tick that
      ended, then zeroes all counters; a callback ON PURPOSE, the user reverted an array-returning
      version as not simpler). Counts saturate at 255. An event whose index is past the counters is
      let through (no panic). `Default`. Type safety (a module only sends its own enum) comes from
      the typed handle behind `ctx.ev`, not from `Throttle`. 11 tests (core-api: 69 + 7 + 10).
    - `CoreEvent::EventsSuppressed` (Err, code 6): the text has source, code and how many dropped
      (the text is built where `new_tick` reports; not written yet).
    - NOT written: the handle behind `ctx.ev` that owns/borrows the source's `Throttle`, calls
      `throttle` before `Ev::send`, turns `new_tick`'s reports into `EventsSuppressed` events, and can
      only send its own module's event enum; the executive calling `new_tick` each tick. The core's
      own events skip the throttle.

26. **`Module` and `Ctx<M>`** (core-api, new files `module.rs` and `ctx.rs`; step 1 of the Ctx
    and exec plan). `trait Module: Sized { type Event: event::Code; fn init(&mut self, ctx: &mut
    Ctx<'_, Self>); fn step(&mut self, ctx: &mut Ctx<'_, Self>); }` (no `command()` yet).
    `Ctx<'a, M: Module> { pub ev: EvHandle<'a, M::Event>, pub reset: ResetInfo, monotonic_us }`;
    `Ctx::new(ev: &mut Ev, throttle: &mut Throttle, source: u8, monotonic_us: u64, reset)` (public
    because the exec is another crate; only the exec calls it); `ctx.monotonic()`. The time is read
    once by the exec and fixed per call (no clock in Ctx). `EvHandle<'a, E: Code>` has private
    fields (`&mut Ev`, `&mut Throttle`, source, time, `PhantomData<E>`) and ONE method:
    `send(&mut self, code: E, text: fmt::Arguments)` = `throttle(&code)`; if over the limit it
    returns without sending (the count is kept in the throttle for `new_tick`), else `Ev::send(source,
    time, code, text)`. `PhantomData<E>` is there because `E` is only used by `send`. Checked by
    hand that these do NOT compile: sending another module's event enum (type mismatch), `ctx.ev.pop()`
    (no such method), setting `ctx.ev.source` or `ctx.monotonic_us` (private). The exec owns each
    module's `Throttle` and the one `Ev` and lends them. 9 tests in `core-api/tests/ctx.rs` (new
    file) play the exec with `run_init`/`run_step` helpers and two test modules (`Counter`, `Noisy`):
    source and time come from the exec, one time per call, state persists across calls, throttle
    applies and reports through `new_tick`, throttles are per source.
    Not written: `command()`, inbound messages, hardware handles, per-module `Ctx` pieces chosen by
    the exec config, and who sends `EventsSuppressed` from `new_tick`'s reports (the scheduler).

27. **Size check** (end of session, nothing in the repo changed for it). Rust: 26 files, 3475 code
    lines: 1140 non-test (core-api 447, core 214, core-types 182, osal 169, core-derive 128) and 2335
    test (33% / 67%); 545 comment lines in non-test code; 677 lines of Markdown. Binary: there is no
    executable yet, so a throwaway probe binary (outside the repo) used the event queue, throttle,
    a module through `Ctx`, and the reset and event logs through `StdFiles`: 352,320 bytes stripped
    with `panic = "abort"` only, 296,600 bytes stripped with `opt-level = "z"`, `lto = true`,
    `codegen-units = 1`. A one-line hello world with the same settings is 287,824, so the Vee code
    in the probe is about 8.8 KB; the rest is Rust's std. The workspace has NO release profile yet
    (add `panic = "abort"`, LTO, `opt-level`, `codegen-units` when the exec binary exists).

28. **OSAL clock** (session 2026-11-18). Files `osal/src/{clock,std_clock,fake_clock}.rs`, same split
    as `Files`. `trait Clock { fn monotonic_us(&self) -> u64 }` (microseconds since the clock
    started, NOT mission time). `StdClock` (`new()` captures an `Instant`; a `u64` overflow, which
    cannot happen in practice, stays at `u64::MAX` rather than panicking). `FakeClock` (`new()`,
    `set`, `advance`; an `AtomicU64`, so `&self` works without a lock). 7 new tests (osal: 39).
    Design decisions made this session, in the discussion before the clock (NOT yet in design.md):
    - The scheduler loop does NOT belong in `core`: `Module` has an associated type and `Ctx<M>` is
      generic, so no `dyn` list or generic list can hold different modules. Only the deployment
      (`exec!`) knows every module type, so `exec!` generates the loop. Hand-write one in a test
      first and design the macro from what repeats.
    - The core is scheduled like a module (every tick, no divisor) but is NOT a `Module` (it owns
      `Ev` and the logs, and its events skip the throttle). ONE `Core` struct owns the services and
      calls them in a fixed order; the services stay separate plain structs. ONE entry point per
      tick, `core.tick(now)`, no begin/end split: the loop comes back around, so drain, watchdog and
      overrun detection (compare consecutive tick start times) can all happen at the top of the next
      cycle. Shutdown needs a separate `stop()` anyway.
    - Loop shape: for each module report its suppressed events (`Throttle::new_tick`, which also
      starts the new tick's count; generic over the module's event enum, so the exec calls it per
      module, and the core provides a generic helper that builds the `EventsSuppressed` text);
      then `core.tick(now)`; then for each module build `Ctx` and `step`.

29. **Clippy lints for flight code** (root `Cargo.toml` `[workspace.lints.clippy]`, all `deny`; the
    four flight crates inherit them; plus a root `clippy.toml` that lets tests use `unwrap`,
    `expect`, `panic!` and indexing). Panic paths: `unwrap_used`, `expect_used`, `panic`,
    `indexing_slicing`, `string_slice`, `unreachable`, `todo`, `unimplemented`,
    `arithmetic_side_effects` (use `checked_`, `saturating_` or `wrapping_`). Casts:
    `cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`. Discipline: `wildcard_imports`,
    `enum_glob_use`, `allow_attributes_without_reason` (every `#[allow]` needs `reason = "..."`),
    `dbg_macro`, `print_stdout`, `print_stderr`, `exit`, `mem_forget`, `let_underscore_must_use`,
    `undocumented_unsafe_blocks`. `cargo clippy --workspace --all-targets` is clean.
    What changed to get there:
    - `#[allow(.., reason)]` only where the code runs at COMPILE time (a panic or overflow there is
      a build error, which we want): `config::parse_or`, the `setting!` macro's `as $ty`, the CRC
      table builder. One more at `crc::table_entry` (a `u8` index into a 256 entry table cannot be
      out of range) and one at `Ev::send` (`let _ = write_fmt`, on purpose).
    - `FixedStr::truncated` copies with `zip` (no indexing, and shorter).
    - New `core-types/src/record.rs` (private): `field::<N, E>(record, offset, error) -> Result<[u8;
      N], E>`; both log decoders use it instead of indexing (N comes from the use, such as
      `u64::from_be_bytes(record::field(..)?)`). A too-short record gives `BadLength` (cannot happen
      after the length check, but it is an error and not a panic).
    - `Queue` uses `checked_add`, `checked_rem`, `saturating_add/sub` and `get_mut`.
    - New `core/src/records.rs` (private): `shape(len, record_len) -> Shape { whole, has_partial }`,
      replacing the same division in both logs' `start`. A `record_len` of 0 gives an empty shape.
      5 tests (core: 46).
    - `reset_log::start`: `get_mut(..read_count)` (the `else` returns `FileError::Other`, cannot
      happen), `usize::try_from(..).unwrap_or(TAIL_RECORDS)`, saturating counters.
    - `osal` IS covered (same lint list, `[lints] workspace = true`). The workspace `unsafe_code`
      is `deny` (not `forbid`), so `osal` can write `#[allow(unsafe_code, reason = "...")]` on a
      specific block; `undocumented_unsafe_blocks` then also requires a `// SAFETY:` comment.
      Every flight crate has `#![forbid(unsafe_code)]` at the top of its lib.rs (a new flight
      crate must add it too), and `forbid` cannot be overridden even by an `allow`. Checked by
      hand: unsafe in core-api fails even with an allow; in osal it fails with no allow, fails with
      an allow and no SAFETY comment, and passes with both. `FakeFiles` has one private `lock()`
      helper that recovers a poisoned lock (`unwrap_or_else(PoisonError::into_inner)`), replacing
      8 `unwrap`s. `std_files` tests have one `remove_dir` helper with the `let _` allow.
    - The `setting!` macro now proves its `value as $ty` cast: it casts back (`narrowed as usize ==
      value`) and asserts the number is unchanged, so the allow's reason is checked at the cast.
    - Ideas not enabled: `as_conversions` (every `as`), `std_instead_of_core`, `missing_docs`.
    Tests now: core 46, core-api 69 + 7 + 9 + 10, core-types 32, osal 39.

30. **`EventsSuppressed` reporter** (`core/src/suppressed.rs`, new file). `suppressed::report::<C, _>(
    &mut Throttle, source: u8, send)` calls `Throttle::new_tick` and, for each event suppressed in the
    tick that ended, calls `send(CoreEvent::EventsSuppressed, fmt::Arguments)` (same callback shape as
    `reset_log::report`; no I/O). Text: `suppressed: source {source} code {code}, {dropped} dropped`,
    at most 46 bytes (fits the default 50). `{code}` is the event's code (not its index). `dropped`
    stops at 255 minus `EV_MAX_PER_TICK`, so it can mean "at least". Generic over the module's event
    enum `C`, so the exec calls it once per module with that module's type; it also zeroes the
    counters, so calling it IS how a new tick starts. No `>=` marker in the text (decided: skip).
    Also: doc comments on `Throttle` and `Ev` saying they are the exec's and `pub` only because
    `Ctx`'s handle `ctx.ev` holds them (modules depend only on core-api; a cycle otherwise).
    7 tests (core: 53).

31. **Core services written down** (`docs/design.md`, new section "Core services" under "Core"; no
    code). Decided with the user, 2026-11-19: the core has seven domains: **Health** (the reset log
    goes INSIDE it, as "related but not a service"; also overrun detection, the watchdog kick,
    core counters), **Time**, **Events**, **Message Bus**, **Command Router** (kept separate from
    the bus), **Tables** and **Files** (the file service, added by the user's decision). Hooks per
    domain are planned (`start`, `tick`, `stop`; the bus has none), not written. One `Core` struct
    owns them, one `core.tick(now)`, order of the domains inside it decided when each is written.
    The per tick loop is in the doc (report suppressed per module, then `core.tick`, then step
    the modules due). Also this session: `EV_QUEUE_DEPTH` default 32 to 1024 (64 KiB; a slot is 64
    bytes; no per source cap, only the per event throttle, by the user's decision; the range
    maximum 65535 stays). Tests now: core 53, core-api 69 + 7 + 9 + 10, core-types 32, osal 39.

32. **Commands are handled in the module's step** (`docs/design.md`; no code). Decided with the user,
    2026-11-19, REPLACING the earlier decision that the exec calls `command()` at the base rate every
    tick. Now: the router puts a command into the target module's queue when it arrives (in the
    core's tick); the module handles its queued commands in its OWN step, at its own rate. So a
    command can wait up to one module period, and a slow module's command queue must cover what
    arrives in that time. The ack for "received" is sent at queueing; the ack for the outcome after
    the module handles it. The `dispatch` budget in the `exec!` example is now part of the step's
    time. NOT designed: how `command()` is called inside the step (before `step()` in the same call
    with one `Ctx`, or another way) and whether a step may handle every queued command or has a
    limit. Edited: Module Requirements, Command Routing, Executive Requirements, the throttling
    paragraph (commands run in the module's step), the Core services table and loop, and the doc
    comment at the top of `core-api/src/throttle.rs`.

33. **The loop test** (`core/tests/exec_loop.rs`, new; first integration test in `core`). A hand
    written executive for three test modules (`Heartbeat`, every tick; `Slow`, every 4th tick,
    reports the time it sees; `Noisy`, every tick, sends `Spam` over the limit) on a `FakeClock`.
    `Slot<M: Module>` (module, `Throttle`, source, period; methods `init`, `report_suppressed`,
    `step`) is GENERIC, so the per module logic is written once. `Exec` has one `Slot` per module
    and the three phases of a tick: (1) each slot's `report_suppressed` (core events at `now`), (2) a
    placeholder for `core.tick(now)`, (3) each slot's `step`, with the fake clock advancing 100 us
    after every slot so each call sees its own time. What this says about `exec!`: it only has to
    generate the struct fields (one `Slot<_>` per module) and the ordered calls in `tick` and `init`;
    no per module logic. 5 tests (all pass; two deliberate breakages of the loop were each caught by
    two tests): init order, source and time; the slow rate (ticks 0 and 4); the time each call sees;
    a noisy module limited per tick and reported at the top of the next (the last tick's report waits
    for the next tick); the order within a tick (core's report, then modules in order). Not covered:
    the core's tick, commands, `start` and `stop`, overrun, the real clock.
    Also: ran `cargo fmt`, which reformatted four committed files (`core-api/src/queue.rs`,
    `core-types/src/event_log.rs`, `osal/src/fake_files.rs`, `core/src/suppressed.rs`; whitespace
    only). `cargo fmt --check` is clean now. Test files outside `#[test]` fns need their own
    `#![allow(clippy::unwrap_used, reason = ..)]` (clippy.toml's exemption covers only `#[test]` fns and
    `#[cfg(test)]` modules). Tests now: core 53 + 5, core-api 69 + 7 + 9 + 10, core-types 32, osal 39.

34. **System log, step 1 of 4** (the `log` facade; design in `docs/design.md`, "System Log"). Chosen
    by the user (option C) after I measured std `sync_channel`/`try_send` alternatives. Done:
    workspace dependency `log` (no default features); `osal/src/syslog.rs` (new): `Line` (Display:
    `<time_us> <LEVEL> <target>: <message>`), a private `Logger` (writes to stderr, ignoring write
    errors on purpose), `init(clock, level)` (one global logger via `OnceLock`, then
    `log::set_logger`/`set_max_level`); 5 tests. `core/tests/exec_loop.rs` calls `init_log()` from
    `run()`, which installs the logger only if `VEE_TEST_LOG` names a level (stderr is not captured by
    `cargo test`, so always installing would print on every run); the exec logs init, each tick, each
    step and each drained event. Try: `VEE_TEST_LOG=debug cargo test -p vee-core --test exec_loop
    a_noisy -- --test-threads=1`. Tests now: osal 44, core 53 + 5. Remaining steps: (2) bounded
    queue (`sync_channel` + `try_send`) + a polling writer thread, switched on when `started` is set;
    (3) mirror events into the log in the Events drain; (4) persisted file plus an in-memory ring
    dumpable by command. Open: lines queued at a panic are lost under `panic = abort`.
    **Step 2a done** (`osal/src/syslog.rs`): `LineBuf` (256 byte fixed buffer holding the FINISHED
    line: text cut at a whole character at `TEXT_MAX` = 254, then `~` if cut, then `\n`; once cut,
    later pieces are ignored; `from_line`, `finish`, `as_bytes`), `Logger::new`, `Logger::emit`
    (formats into a `LineBuf` in both modes, then `queue` empty = one `write_all` to stderr;
    filled = `try_send`, and on failure `dropped += 1`), `log` is now the level check plus `emit`.
    SPLIT (user's request, applies to the whole OSAL): interface files use only `core` (`clock`,
    `files`, `syslog`: `Line`, `LineBuf`), implementation files are named for the platform or the
    double (`std_*`, `fake_*`). `syslog.rs` was mixed, so the logger, `init`, stderr and the queue
    moved to the new `osal/src/std_syslog.rs`; `init` is now `std_syslog::init`. Rule recorded in
    `osal/src/lib.rs` and in design.md's OSAL Requirements. Tests: 6 in `syslog`, 3 in
    `std_syslog`.
    **Step 2b DONE** (`osal/src/std_syslog.rs`): `start_queue()` (global) and `Logger::start_queue`
    (makes `sync_channel(QUEUE_DEPTH)`, spawns the writer with `thread::Builder` so a failure is an
    `Err` and not a panic, THEN fills `queue`); `StartError { NotInstalled, AlreadyStarted,
    Thread(io::Error) }`; `Logger::drain` (one writer pass: at most `QUEUE_DEPTH` lines, then a
    Warn line "N lines dropped: the queue was full" straight to the output; returns false when the
    queue is closed); the writer loops `drain` + sleep `POLL` (10 ms). The user chose to handle a
    failed spawn (stay in direct mode) AND to make forgetting `start_queue` impossible: the
    executive's start sequence (not written yet) must be one function that calls `start_queue`
    then sets `started`, and report any `Err` with `log::error!` plus an event. Written in
    design.md. Tests: 8 in `std_syslog`. NOT tested: the real thread writing to the real stderr
    (only `start_queue` returning Ok and the mode switching); lines still queued at process exit or
    at a panic are lost. The old warning (unused `QUEUE_DEPTH`) is gone. OLD NOTE (done now),
    expected warning: `QUEUE_DEPTH` unused. **2b still to do**: a
    `start_queue()` that creates `sync_channel(QUEUE_DEPTH)`, fills `queue`, and spawns the polling
    writer thread (`try_recv` + sleep; writes each line to stderr; also reports and clears
    `dropped`); handle a failed thread spawn by staying in direct mode; call it before `started`,
    since spawning allocates. The writer loop should take an `impl io::Write` so it can be tested.

35. **Events into the system log (step 3), text size 64, `EventsDropped`**. Decided with the user:
    mirror each event at SEND time in `Ev::send` (`core-api/src/ev.rs`; after the throttle, before
    the queue), not in a drain, so an event a full queue drops is still in the log, and lines are in
    order with the rest of the log. `core-api` now depends on `log`. Line: `INFO|WARN|ERROR event:
    src=3 code=1 t=10200 us: spam` (Err is ERROR; `log_level(severity)`). The drain therefore does
    not log. `EV_TEXT_MAX_BYTES` default 50 to 64: the log record is 78 bytes (was 64) and an `Event`
    slot is 80 bytes (was 64), the queue 80 KiB. Docs updated (design.md, `ev.rs`, `suppressed.rs`,
    `sample-deployment/.cargo/config.toml`); old numbers in the history items above are history.
    New `CoreEvent::EventsDropped` (Err, code 7) and `core/src/events.rs` (new): `report_dropped(ev,
    now)` sends it from `ev.take_dropped()` (text `N events dropped: the queue was full`, at most
    45 bytes). The caller must call it AFTER emptying the queue, or the report itself can be dropped
    (then it is counted for the next call). The drain (the pop loop that hands events to the event
    log and downlink) is NOT written: no sinks exist (needs the file service and the bus). Tests:
    `core-api/tests/ev_log.rs` (new, 4 tests: its own program, a capturing logger, one test fills
    the queue and checks the overflowing event is still logged), `core/src/events.rs` (4), `core_event`
    tests updated. Open: the log's queue (256) is smaller than the event queue (1024), so a burst of
    more than 256 events at once overflows the log's queue (reported as "N lines dropped"); decide
    whether to raise `QUEUE_DEPTH`. Step 4 (persisted file plus in-memory ring) is not started.
    `exec_loop.rs` no longer logs in its drain; it does not call `report_dropped` (nothing empties
    the queue until the end of a test).

36. **`Core` with `tick(now)`** (`core/src/lib.rs`; `vee_core::Core<F: files::Files>`). Chosen by the
    user over a sink closure: write the events to the event log, blocking, for now. Fields: `pub ev:
    ev::Ev` (the executive lends it to every `Ctx`), `files: F` (read back through `files()`), and
    `event_log_path: &'static str`; `Core::new(files, path)` (the caller runs `event_log::start`
    first). `tick(now)` = `write_events()` (pop every event and `event_log::append` it; a failed
    append loses that event, still empties the queue, counts failures and reports ONCE per tick with
    `log::error!` and the first error) then `events::report_dropped(&mut self.ev, now)`. The
    blocking `append` is TEMPORARY (the design forbids blocking in the loop); the file service
    will replace it with a request and the loop stays. `core/Cargo.toml`: `log` is now a normal
    dependency. 4 tests in lib.rs (order and emptying, nothing queued, dropped events reported
    AFTER emptying and written next tick, a failing file system still empties the queue).
    `core/tests/exec_loop.rs` now uses the real `Core<FakeFiles>`: `Exec` has `core` instead of
    `ev`; step 2 of `tick` is `self.core.tick(now)`; `Exec::logged()` (was `drain`) runs one more
    core tick (the last tick's module events are still queued) and decodes the records from the fake
    event log. The 5 loop tests are unchanged apart from `.drain()` to `.logged()`. A module named
    `core` is impossible (it would clash with Rust's `core`), hence `Core` lives in `lib.rs`. Not
    done: `Core` does not own the reset info or the clock yet (the exec still holds them).

37. **Startup: `Core::start`** (`core/src/lib.rs`). `Core::new` is now PRIVATE; `Core::start(files,
    reset_log_path, event_log_path, now) -> Core` is the one way to make a core. Order: (1)
    `reset_log::start` + `reset_log::report` (the reset report is the first event of every run);
    (2) `event_log::start` + `event_log::report`. `Core` now owns the `ResetInfo` (`core.reset()`),
    so Health's piece is in; the loop test's `Exec` no longer holds its own. User decision (asked):
    startup does NOT fail: on a `FileError` the core carries on and sends a new Err event, with the
    error in the text: `CoreEvent::ResetLogUnavailable` (code 8; modules then get
    `ResetInfo::first_boot()`, which hides a reset loop) and `EventLogUnavailable` (code 9). Tests: 5
    new in lib.rs (first boot, second boot after an unclean run, a partial event log rotated and
    reported after the reset report, both logs failing, startup events written by the first tick);
    `core_event` tests for codes 8 and 9. Loop test: `Exec::new` calls `Core::start`; its expectations
    got the core's reset report as the first event (the noisy test filters the core's events by
    code). The full startup order is in design.md (Executive Requirements): syslog init, `Core::start`,
    module inits, `start_queue`, `started`, first tick. NOT written: the exec's one start function
    (steps 1, 3 and 4 are the exec's), `started` and the allocation guard, the stop path
    (`reset_log::stop`). `Core::start` has a TIME parameter only for the startup events.

38. **Shutdown: `Core::stop(now, reason)`** (`core/src/lib.rs`). (1) `reset_log::stop` records the
    clean shutdown (so the next boot sees `Clean(reason)`, not `Unknown`); on a `FileError` it sends
    `ResetLogUnavailable` through a helper shared with `start` (`send_reset_log_unavailable`; the
    event's doc now says "at startup or at shutdown"); (2) `self.tick(now)` TWICE: the first writes
    the queue and queues the dropped-events report, the second writes it (no second copy of the
    drain). `&mut self`, not consuming: the doc says do not tick after it. `Core` now stores
    `reset_log_path` too (`Core::new(files, reset_log_path, event_log_path)`, private). Tests (3,
    in lib.rs): a stop then a new `start` on a copy of the reset log sees `Clean(Commanded)`, count 2,
    0 unclean; stop writes all queued events plus the dropped report; with a double where only the
    reset log fails (`ResetLogDown`), stop sends event 8 and the event log has it after the earlier
    event. Core tests: 69. design.md: a "Shutdown" bullet in Executive Requirements. NOT done: nothing
    calls `stop` yet (the exec's signal handling and the commanded-shutdown path are future work),
    and the loop test now does (see below).
    Loop test added (`core/tests/exec_loop.rs`, 6 tests now): `a_clean_stop_writes_the_last_ticks_events_
    and_the_next_start_sees_a_clean_end`: after 2 ticks the last tick's module events are queued, not in
    the log; after `Exec::stop` they are all in it and the queue is empty; a new `Core::start` on a copy
    of the reset log sees `Clean(Commanded)`, count 2. `Exec` got `stop()` and `event_log()` (a pure
    read); `logged()` is `event_log()` after one more core tick.

39. **Decisions on the allocation guard and panics (no code)**, with the user. (a) The guard is a
    DEV/SIL/CI check, not a flight protection: behind a Cargo feature, off in flight builds (an
    allocation after startup that succeeds is a bug to find before flight, and aborting would turn it
    into an outage); where on, it aborts with a fixed message (a panic could allocate inside the
    allocator). The OSAL provides the `GuardAlloc` type, the deployment's `main` declares
    `#[global_allocator]` (one per program). Counting mode for tests (per-thread flag, assert 0
    allocations around `emit`, `drain` and `Core::tick`). NOT written. (b) Flight builds keep
    `panic = "abort"` (fail-stop: no zombie threads, no poisoned locks, no `Drop` code writing a
    false `Shutdown`); dev and test builds unwind. No `catch_unwind` per module for now. (c) The
    panic hook runs BEFORE the abort, so a "last gasp" is possible there: see the system log's step 4
    (in-memory ring of recent lines, dumped to a file by the hook; no allocation, no locks held
    by others, bounded, never touch the reset log). design.md: "No allocation" bullet and Faults.

40. **Panic hook rules (no code)**, recorded in design.md (Faults) after the user asked "any way it could
    bite us?" about the `PANICKING` flag idea (the feed to the systemd watchdog stops while a panic is
    in progress, so a stuck hook on a non-main thread cannot be hidden by a ticking main loop).
    Rules: only in flight builds (`cfg(panic = "abort")`, or a `should_panic` test would leave the flag
    set); a latch with `compare_exchange` (the first panicker dumps, a second one parks and does NOT abort,
    so the dump is not cut short); installed first thing in `main`; no allocation, no waiting on locks,
    one bounded write, never panic, never touch the reset log; `READY=1` and a `WatchdogSec` longer than
    the worst startup; an uninterruptible blocked write is an OS limit; keep the hook thin and test its
    logic. Not written.

41. **Allocation guard** (`osal/src/std_alloc_guard.rs`, new; the first `unsafe` in the project) and the
    system log's `QUEUE_DEPTH` raised from 256 to 1024 (same as the event queue; 256 KiB; design.md
    updated). `GuardAlloc` implements `GlobalAlloc` by forwarding to `System`; it overrides only `alloc` and
    `dealloc` (the provided `alloc_zeroed` and `realloc` call `alloc`, so they are counted and checked
    too). Every `alloc` bumps a per THREAD counter (`thread_local!` with a `const` `Cell`, so it never
    allocates), read with `allocations_on_this_thread()`; and if `arm()` has been called (a static
    `AtomicBool`, never cleared) it writes a fixed message to stderr and `process::abort()`s. The unsafe
    impl has `#[allow(unsafe_code, reason)]` and a `// SAFETY:` comment, as item 29 requires.
    DEVIATION from item 39 and the earlier design text: NO Cargo feature. The type does nothing unless a
    program declares it with `#[global_allocator]`, so the deployment's `main` is the switch; a feature
    would be a second switch for the same thing. design.md changed to say so. Tests: 4 in `std_alloc_guard`
    (counted, free and arithmetic not counted, `Vec` growth counted, each thread its own count); 3 in
    `std_syslog` (`emit` queued, `emit` direct and `drain` into `io::sink()` allocate NOTHING; direct mode
    needs one warm up write first); `osal/tests/alloc_guard_abort.rs` (new, 2 tests): the abort cannot be
    tested in process, so the test runs its own test binary as a child (`current_exe`, `--exact`, an env
    var marks the child), arms, allocates, and the parent checks SIGABRT and the message (unix only).
    The osal's unit test program declares `GuardAlloc` in `lib.rs` under `#[cfg(test)]` (never armed).
    NOT done: `Core::tick` is not asserted allocation free, because `FakeFiles::append` allocates (a
    `Mutex<Vec>`) and the temporary blocking `append` goes away with the file service; assert it then.
    `arm()` is called by nobody yet: the exec's one start function (step 4 of the start sequence) will.
    Considered and rejected: an "already aborting" flag in the guard's abort path (recursion if the message
    write allocates, or a hang on stderr's lock). The guard is not in flight builds, so it is not worth the
    code. The same worry is real for the PANIC HOOK, which does run in flight: see item 40 (latch).
    osal: 62 + 2.

42. **Double panic checked** (no code): a throwaway program showed that a panic inside the panic hook aborts the
    process at once (exit 134, SIGABRT) in both `panic = "abort"` and unwinding builds, so the latch needs no
    extra flag for the same thread re-panicking. Recorded in design.md (Faults, after the latch bullet).

43. **The ring was written and then REMOVED, and the last gasp FILE dropped** (2026-11-19, the user's decisions).
    `Ring<N>` in `osal/src/syslog.rs` was done and tested (6 tests), then deleted (`git checkout` of the file;
    nothing else in it had changed). Then the last gasp file was dropped too: the journal is binary (only
    `journalctl` or sd-journal can read it), but systemd can send stderr to a plain text file
    (`StandardError=append:`, systemd 240+, NOT `file:`, which truncates at start) with a logrotate rule
    (`copytruncate`) to cap it. So the panic hook is: the `PANICKING` flag (set first thing, a plain store;
    NO latch, decided later the same day: see item 46), `is_panicking()` for the watchdog feed, and ONE stderr line `thread '<name>' at <file>:<line>:<col>:
    <message>` built with `syslog::LineBuf::from_line` (same format and clock as the log, one write). No file, no
    path, no next boot step. Boxing the hook allocates nothing if the closure captures nothing (a zero sized
    type): use statics (`&'static str`, `OnceLock<StdClock>`, an `AtomicBool`) and assert zero allocations with
    `std_alloc_guard` (expectation, not yet measured). The core does not read the stderr file: ground asks for
    its tail with the file-dump command (not written). The sample deployment will carry the unit and logrotate
    snippets (not written). System log step 4 is therefore: the hook, then E (the watchdog feed stops while
    `PANICKING`, Health's piece). design.md updated (Faults, System Log).

44. **Panic hook C1: the report line** (`osal/src/panic_report.rs`, new, interface file, `core` only). `Location
    { file, line, column }` and `line(time_us, thread: Option<&str>, location: Option<&Location>, message:
    Option<&str>) -> syslog::LineBuf`: `<time> ERROR panic: thread '<thread>' panicked at <file>:<line>:<col>:
    <message>`, built with the existing `Line` and `LineBuf` (same format, clock, 254 byte cut with `~`). A
    missing part shows a placeholder (`<unnamed>`, `<unknown location>`, `<message is not text>`), so the
    line always has the same shape. A multi-line message (an `assert_eq!` failure) keeps its lines on
    purpose: the hook writes the line in one write, so they stay together. 6 tests (osal: 68 + 2). Not
    written yet: C2 the flag (see item 46), C3 the hook that gets the parts from `PanicHookInfo` and writes to stderr, C4
    install, C5 the child process test. `osal/src/lib.rs` doc lists `panic_report` as an interface file.

45. **OSAL layout: the std implementations moved to `osal/src/linux/`** (user's request). `linux/mod.rs` (new, with
    a doc comment) declares `std_alloc_guard`, `std_clock`, `std_files`, `std_syslog`. File names unchanged
    (the `std_` prefix stays, so `linux::std_clock` is not confused with the interface `clock`). Interfaces
    (`clock`, `files`, `syslog`, `panic_report`) and the test doubles (`fake_clock`, `fake_files`) stay in
    `osal/src/`. New paths: `vee_osal::linux::std_syslog` (and the others), used by
    `core/tests/exec_loop.rs` and `osal/tests/alloc_guard_abort.rs`; inside osal, `crate::linux::std_clock`.
    `linux` is NOT behind a `cfg(target_os = "linux")` (so it still builds on a dev machine that is not
    Linux; `alloc_guard_abort.rs` is unix only already). Older items above use the old paths: they are
    history. design.md's OSAL Requirements mention the directory. `git mv` staged the three renames
    (nothing else is staged). Same tests pass (osal 68 + 2).

46. **The latch is dropped** (2026-11-19, the user's question and decision; design.md Faults updated). The hook just
    sets `PANICKING` (a plain `AtomicBool` store) first thing, for the watchdog feed (`is_panicking()`), then
    builds the line (`panic_report::line`) and writes it to stderr in one write. Two simultaneous panics both run
    the hook; each line is one write so they do not mix; whichever finishes first aborts, so the other's line
    may be missing (acceptable). Parking the losers would also have stopped a second thread from aborting a
    process whose first hook is stuck. No `Latch` type, no `claim`, no parking. Items 40 and 42 above mention
    the latch: history. C2 is now just the flag and `is_panicking()`, in `osal/src/linux/std_panic.rs` with the
    hook (C3).

47. **The panic hook is written** (`osal/src/linux/std_panic.rs`, new; C2 to C4 of item 44, with item 46's change).
    `install(clock: StdClock) -> Result<(), InstallError>` (`AlreadyInstalled` on a second call; the first hook
    stays), `is_panicking()` (reads the static `PANICKING`), and the private `hook(&PanicHookInfo)`: sets
    `PANICKING` (a plain store) first, reads the time from the static `CLOCK` (`OnceLock<StdClock>`, filled by
    `install` BEFORE `set_hook`, so `map_or(0, ..)` is only for the impossible case), gets the thread name
    (`thread::current().name()`), the location and `info.payload_as_str()`, builds the line with
    `panic_report::line` and does one `write_all` to `io::stderr()`. The user chose option 1: the clock is
    passed to `install`, as `init` does (a copy of the same one). `Box::new(hook)` allocates nothing: `hook` is
    a function item (zero sized, no captured data). MEASURED, not just expected: the child process test asserts
    `install_allocations=0`. Checked with throwaway programs: with edition 2024, `payload_as_str` gives the
    FORMATTED text (`boom 7`, and an `assert_eq!` message), in `panic = "abort"` and unwinding builds; it
    allocates nothing itself (std builds the `String` before it calls the hook, outside our control); a
    `panic_any(42)` payload has no text and the report says `<message is not text>`. (A first experiment
    with plain `rustc` used edition 2015, where `panic!("x {y}")` with one argument is NOT a format string
    and showed the template: not a Vee problem, which is edition 2024.) The hook takes stderr's lock for its
    write (std gives no `try_lock` on stderr; avoiding it would need `unsafe` on fd 2): design.md says so.
    Test: `osal/tests/panic_hook.rs` (new, 1 test, its own program, child process like the guard abort test):
    the child checks `panicking_before=false`, `install_ok`, `install_allocations=0`, second install is
    `Err(AlreadyInstalled)`, a named thread `worker` panics with `boom 7`, `panicking_after=true`; the parent
    checks exactly ONE line containing ` panic: `, `ERROR panic: thread 'worker' panicked at`, `panic_hook.rs:`
    and `: boom 7`. The file needs `#![allow(clippy::panic, print_stdout, unwrap_used,
    arithmetic_side_effects, reason)]` (its helper is not a `#[test]`). `linux/mod.rs` doc mentions
    `std_panic`. Tests: osal 68 + 2 + 1. NOT done: call `install` (the exec's start function does), the
    `panic = "abort"` release profile, the watchdog feed reading `is_panicking`, a test for two simultaneous
    panics, `cfg(panic = "abort")` gating (the exec's `main` decides).

48. **Watchdog scoped, shedding removed** (no code; decisions with the user 2026-11-19; design.md updated in
    Faults and the `exec!` example). (a) Software watchdog feed: `Watchdog` trait in the OSAL with `ready()` and
    `feed()` only (no `interval_us`); `Core::tick` calls `feed()` EVERY tick and the implementation rate limits
    itself (at most once per half `WATCHDOG_USEC`), non-blocking socket, withholds while `is_panicking()`
    (option A: the rule lives in the Linux impl, not the core), `WATCHDOG_PID` must match, no watchdog
    configured is not an error. A feed or `READY=1` failure is one Err event per streak (`CoreEvent::
    WatchdogUnavailable`, planned code 10). `Core<F>` becomes `Core<P: Platform>` (W3a, a refactor with no
    behaviour change). Steps W1 interface and fake, W2 `linux/std_watchdog.rs` (parsing as pure functions, tests
    with a real `UnixDatagram` in a temp dir, no `env::set_var`), W3a, W3b, W4 event, W5 docs and the sample unit
    (`Type=notify`, `WatchdogSec`, `Restart=on-failure`, `StartLimitBurst`, `StartLimitAction=reboot-force`,
    `StandardError=append:`). W6 (exec calls `ready()`) waits for the exec. NOT started. (b) The man page
    shows the watchdog starts after startup completes, so design.md's "WatchdogSec longer than startup" line
    was wrong and is fixed. (c) HARDWARE watchdog: NOT designed (user: a later mission problem). If wanted, likely a module over an
    OSAL device handle that withholds while panicking. (d) SHEDDING REMOVED: no `shed`, `sheddable` or
    `essential` in the `exec!` example; an overrun is detected and reported only. After an overrun the loop SKIPS to the
    next boundary (user's decision) and the overrun event says how many ticks were skipped (not written). Open: tick period against
    `WATCHDOG_USEC / 2` check at startup (the exec's start function).

49. **Watchdog W1: the interface and the fake** (`osal/src/watchdog.rs` and `osal/src/fake_watchdog.rs`, new).
    `trait Watchdog { fn ready(&self) -> Result<(), WatchdogError>; fn feed(&self) -> Result<(), WatchdogError>; }`
    (both `&self`, neither may wait; `feed` is called every tick and the implementation decides when to send, and
    returns `Ok` without sending when none is due or while panicking; no watchdog configured is `Ok` too).
    `WatchdogError { NotFound, PermissionDenied, ConnectionRefused, QueueFull, Other }`: the first three are
    std's kinds, `QueueFull` is std's `WouldBlock` said the way it applies (the Linux impl maps it), and each
    `Display` text says what is wrong in watchdog terms (`supervisor queue is full`, `nothing is listening on
    the supervisor socket`, ...) because it ends up in the event. A failed send must NOT advance the
    rate limit's last-sent time, so the next tick retries (W2). With `Display` and `core::error::Error`. `FakeWatchdog` (atomics, no
    lock): `new`, `set_failing(bool)` (both calls then fail with `Other`; failed calls are still counted),
    `ready_calls()`, `feed_calls()`. 6 tests (osal: 74 + 2 + 1). Also this session: design.md and item 48
    updated with the user's two decisions: the hardware watchdog is NOT designed (a later mission problem), and
    after an overrun the loop SKIPS to the next boundary and says how many ticks it skipped (not written). Next:
    W2 `osal/src/linux/std_watchdog.rs`.

50. **Watchdog W2: the Linux implementation** (`osal/src/linux/std_watchdog.rs`, new). `StdWatchdog::from_env(clock)
    -> Result<StdWatchdog, StartError>` reads `NOTIFY_SOCKET`, `WATCHDOG_USEC`, `WATCHDOG_PID`; the checking is the
    pure `parse(notify_socket, watchdog_usec, watchdog_pid, own_pid) -> Result<Config, StartError>` (so tests do
    not touch the environment; `env::set_var` is unsafe in edition 2024). `StartError { InvalidSocketName,
    InvalidWatchdogUsec, InvalidWatchdogPid, Socket(io::Error) }`: a bad value is an error, not a guess (a socket
    name must start with `/` or `@name`; `WATCHDOG_USEC` 0 means no watchdog; a `WATCHDOG_PID` that is not ours
    means no watchdog for us but `ready` still goes out; nothing set means not run by systemd: both calls do
    nothing and return `Ok`). The socket is a NON-BLOCKING `UnixDatagram` and the destination a `SocketAddr`
    (`from_pathname` or `from_abstract_name`), both made once in `new`, so a send is one `send_to_addr` with no
    allocation (a test asserts zero). `ready` sends `READY=1` (not rate limited). `feed` = `feed_at(now,
    is_panicking())`; `feed_at` returns `Ok` without sending when there is no watchdog, when panicking, or when
    `now < next_due_us`; else sends `WATCHDOG=1` and, ONLY if the send worked, sets `next_due_us = now +
    interval/2` (`FEEDS_PER_INTERVAL` = 2, systemd's advice), so a failed feed is retried at the next call. An
    allowed feed after a panicking one still goes out at once (the withheld one did not use the slot). `From<
    io::Error> for WatchdogError` maps NotFound, PermissionDenied, ConnectionRefused, WouldBlock (to
    `QueueFull`), the rest to `Other`. 17 tests (osal: 91 + 2 + 1): the parsing, a real `UnixDatagram` in a
    temp dir (ready, first feed, rate limit, feeding every 10 ms tick sends twice per 10 s timeout, panicking
    sends nothing, no watchdog for us, an abstract name, NotFound, ConnectionRefused, a queue filled until
    `QueueFull` and then the feed goes out once the receiver has read, error mapping, zero allocation).
    NOT tested against a real systemd. Next: W3a (`Core<P: Platform>`), W3b (the feed in `tick`).

51. **Watchdog W3a: `Core<P: Platform>`** (a refactor, no behaviour change). New in the OSAL: `platform.rs`
    (interface, `core` only): `trait Platform { type Files: Files; type Watchdog: Watchdog; }`, a type that only
    NAMES the services (never made, used as a type argument); `fake_platform.rs`: `FakePlatform<F = FakeFiles>(
    PhantomData<F>)` with the fake watchdog (`Core::<FakePlatform>` is the usual test core,
    `Core::<FakePlatform<FailingFiles>>` a special one; `PhantomData` is how a struct says it is about `F`
    without holding one); `linux::LinuxPlatform` in `linux/mod.rs` (`StdFiles` and `StdWatchdog`). In the
    core: `Core<P: platform::Platform>` has `files: P::Files` and `watchdog: P::Watchdog` (stored, not used
    yet), `start(files, watchdog, reset_log_path, event_log_path, now)` and the private `new` take the
    watchdog, new accessor `watchdog()` (for tests, like `files()`). Callers now NAME the platform:
    `Core::<LinuxPlatform>::start(..)`, because Rust cannot infer `P` from `P::Files`. core tests: new helper
    `core_on(files)` (an unstarted core on any file system), `type Fake = FakePlatform`; `core/tests/
    exec_loop.rs` uses `Core<FakePlatform>`. Same test counts pass (core 69 + 6, osal 91 + 2 + 1).
    Next: W3b, the feed in `Core::tick`.

52. **Watchdog W3b: the feed in `Core::tick`, with its error event** (W4 folded in, because a feed call whose
    `Err` is ignored breaks the fail-loudly rule). `Core::tick` ends with `self.watchdog.feed()` (LAST, so a
    tick that hangs part way is not fed) and `Core::ready(now)` (new, public: the exec calls it after the
    module inits and before the first tick) calls `watchdog.ready()`; both pass the result to the private
    `note_watchdog(now, result)`: `Ok` clears the `watchdog_failing` flag; an `Err` when the flag is clear sets it
    and sends `CoreEvent::WatchdogUnavailable` (new, Err, code 10, text `watchdog: <WatchdogError text>`, at most
    ~56 bytes); an `Err` when it is already set sends nothing. So a streak is one event, and a failed `ready`
    followed by failing feeds is one event. The feed is retried every tick (the implementation does not
    advance its due time on a failure). The event is queued at the end of the tick and written by the next one.
    The user said NO recovery event. 4 new tests in `core/src/lib.rs` (feeds once per tick; `ready` once and no
    event when it works; a failing watchdog reported once per streak, fed every tick, and a new streak reported
    again; a failed `ready` plus failing feeds is one event) and the code 10 check in `core_event` (core 73 + 6,
    core-types 32). design.md: start sequence gets step 5 `core.ready(now)` and the first tick becomes step 6;
    the Core services table's Health row. NOT done: the `StdWatchdog::disabled` fallback for when
    `from_env` fails at startup (the exec's start function needs it and reports the `StartError`), the exec's
    start function itself (calls `from_env`, `ready`), the sample unit file, the tick period against
    `WATCHDOG_USEC / 2` check, `Core<P>` with a real `LinuxPlatform` anywhere (nothing makes one yet).

53. **`ready` / `READY=1` REMOVED** (user's question 2026-11-19: "is it sensible to notify the watchdog?"; the man page
    confirms `Type=simple` counts as started at once, the watchdog timer starts with the process, and
    `WatchdogSec=` makes `NotifyAccess=` default to `main`). Gone: `Watchdog::ready`, `FakeWatchdog::ready_calls`
    and `ready`, `StdWatchdog::ready` and the `READY` constant, `Core::ready`, the start sequence's step 5 (the
    first tick is step 5 again), the "failed ready" test and the `ready` test in core, and the `ready` uses in the
    `StdWatchdog` tests (the full queue test now fills the queue with `feed_at` a half timeout apart; a
    connection refused and an abstract socket use `feed_at`; a watchdog for another process now sends nothing at
    all). `Watchdog` is one method, `feed`. The unit file is the default `Type=simple` with `WatchdogSec=` longer
    than the worst startup (the watchdog's detection time is at least that); feeding between the module inits is
    possible later at no cost. Items 48 to 52 above that mention `ready`, `READY=1` or `Type=notify` are history.
    W6 is gone. Tests: core 71 + 6, osal 90 + 2 + 1, core-types 32, core-api 69 + 7 + 9 + 10 + 4.

54. **Exec start and the watchdog fallback: decisions, and the fallback** (2026-11-19, with the user). Decided:
    (1) If the watchdog cannot be set up, carry on (no refusing to start): `StdWatchdog::disabled(clock)` (new,
    2 lines of state, `feed` always `Ok`), and the executive reports the `StartError` once (`log::error!` and a
    `WatchdogUnavailable` event, sent through the public `core.ev` after `Core::start`, since the error exists
    before the core does). (2) Hand-write the executive's start function in a `sample-deployment` binary, to
    track it: NOT started, see below. (3) Log how long startup took (not written). (4) The supervisor watchdog
    is optional and low level: the core is NOT built around it, so NO tick period check, no `interval` accessor,
    nothing at startup. (5) NO signal handler: a SIGTERM from systemd ends the run `Unknown`; systemd's own log
    records it; accepted ("let's not overbuild"). design.md updated (three bullets before "Startup does not
    fail"). Written: `StdWatchdog::disabled` and one test (osal: 91 + 2 + 1).
    PLAN for the sample deployment (one piece at a time): S2a the crate (`sample-deployment`, a workspace
    member, `vee-sample-deployment`; inherits the workspace lints; the existing `.cargo/config.toml` with
    the settings stays) and a `start` that does steps 1 to 5 of the start sequence with `LinuxPlatform`, no
    modules; S2b a SAMPLE MODULE (the user: a real example, not a fake sensor; it should grow to show every normal
    module operation, so name it for what it is, such as `example`, and start with init, step and an event); S2c the loop (an absolute tick deadline, so a late
    tick SKIPS to the next boundary: `next = (t / period + 1) * period`, logged with `log::warn!`), and an optional
    `VEE_SAMPLE_TICKS` so a run can end through `Core::stop` (Commanded); S2d a test that runs the built binary
    twice in a temp directory and checks the second boot's reset report; S2e the release profile
    (`panic = "abort"`, LTO), the guard declared under a feature, and the sample unit and logrotate files.

55. **Sample deployment S2a: the start function** (`sample-deployment/`, new workspace member `vee-sample-deployment`,
    `Cargo.toml` and `src/main.rs`; inherits the workspace lints, `#![forbid(unsafe_code)]`). `start(clock) ->
    Result<Core<LinuxPlatform>, StartError>`: (1) `std_syslog::init(clock, LOG_LEVEL = Info)`; (2) `std_panic::
    install(clock)` when `cfg!(panic = "abort")`; (3) `StdWatchdog::from_env(clock)`, or `disabled(clock)` plus the
    error kept; (4) `Core::<LinuxPlatform>::start(StdFiles, watchdog, RESET_LOG, EVENT_LOG, now)`, then the
    deferred watchdog error is reported (`log::error!` and `WatchdogUnavailable`, sent through `core.ev`); (5) module
    inits (none yet, a marked spot); (6) `std_syslog::start_queue()` (a failure is `log::error!` and the new
    `CoreEvent::SystemLogQueueUnavailable`, Err, code 11, and the logger stays direct) and `std_alloc_guard::
    arm()`; then `log::info!("started in N us")`. `StartError { SystemLog(SetLoggerError), PanicHook(
    InstallError) }` only for bugs in the start function (a logger or hook already installed); `main` returns
    `Result<(), StartError>`, so Rust prints the error and exits non-zero without a print macro. `main` today:
    `start`, ONE `tick`, `stop(Commanded)` (temporary, until the loop). Logs go to `reset.log` and `events.log` in
    the current directory. Also: `std_syslog::StartError` got `Display`. Run by hand twice in a temp dir: the
    second boot's reset report says `prev end commanded`; startup took 5 to 10 ms (file creation and syncs).
    The event's own time (`t=214 us`) is the `now` that `Core::start` was given; the line's time is when it was
    written (later, after the reset log I/O): both are right. Not tested yet (S2d). Not done: the loop (S2c).
    Build-time settings only apply when cargo runs from `sample-deployment/` (the doc comment says so).

56. **Sample deployment S2b: the example module** (done in three small steps). (1) New crate `modules/example`
    (package `vee-example`, a workspace member; `#![no_std]`, `#![forbid(unsafe_code)]`; depends ONLY on
    `vee-core-api`, as a module should): `Example` (state: `steps`), `ExampleEv { Started, Stepped }` (both Info,
    `#[derive(event::Code)]`), `impl Module`: `init` sends `started, previous end: <ctx.reset.previous>`, `step`
    counts and sends `step N at T us`. Written to be read: the doc comments explain what a module is, what the
    derive generates and what `#[repr(u16)]` is for. It is meant to GROW, one small working use of each module
    capability (commands, tables, bus, files) as they exist. 2 tests with a small harness that plays the
    executive. (2) `Slot<M>` MOVED from `core/tests/exec_loop.rs` to `vee_core::slot` (`core/src/slot.rs`, now
    `pub`; second user, so extracted; fields stay private, methods `new`, `init`, `report_suppressed`, `step`
    are public); the loop test uses it, same 6 tests pass. (3) `sample-deployment/src/main.rs`: `Exec { clock,
    core, tick, example: Slot<Example> }` (the part `exec!` will generate) with `tick()` (suppressed reports,
    `core.tick`, module steps) and `stop()`; `start` now returns `Exec` and does the module init in its marked
    spot (source `EXAMPLE_SOURCE` = 1, `EXAMPLE_PERIOD_TICKS` = 100, i.e. 1 Hz at 100 Hz). Run twice by hand: the
    second boot's example `init` says `previous end: commanded` and its first step event follows. `main` is
    still `start`, one `tick`, `stop`. Next: S2c the loop. design.md (Module Requirements) and the doc of
    `vee_core` mention `modules/` and `Slot`. Tests: 12 test binaries pass, including `vee-example` 2.

57. **The watchdog's rate limit REMOVED** (the user's call, after the measurement: a non-blocking `send_to` is about
    2.5 us, 0.025% of a core at 100 Hz; the other reasons for a limit were PID 1 wakeups, noise if `NotifyAccess` is
    misconfigured, and a higher tick rate, all weak). `StdWatchdog` is now `{ link: Option<Link> }` and nothing else:
    no clock, no `next_due_us`, no half-interval constant. `from_env()` and `disabled()` take no arguments (the
    sample's two calls lost their `clock`). The parsing is `keep_alive_address(notify_socket, watchdog_usec,
    watchdog_pid, own_pid) -> Result<Option<Address>, StartError>`: it still checks all three variables (a bad one is
    an error), and `WATCHDOG_USEC` is read only to tell whether a watchdog exists (0 or unset: none; a `WATCHDOG_PID`
    that is not ours: none); no `NOTIFY_SOCKET` means nothing to send to. `feed_at(now, panicking)` became
    `feed_unless(panicking)`, which sends on every call; a failed send is just retried by the next tick. Tests:
    the rate limit ones are gone, and `every_feed_sends_a_keep_alive` is new (osal: 88 + 2 + 1). Docs: `Watchdog::feed`,
    `Core::tick`, design.md. Items 48, 50 and 54 above mention the limit: history.

58. **The software bus designed** (a long discussion with the user, 2026-11-19; design.md, "Software Bus" rewritten;
    NOTHING WRITTEN). Decisions: a topic has a name, a type and an explicit APID (`u16`, unique, checked at compile
    time; also the CCSDS APID on downlink); messages are `Copy` plain data up to about 3 kB with a
    `#[derive(bus::Message)]` (wire encoding big endian in declaration order; a field's index is its position in
    that layout, NO pinned field ids, by the user's call: it must agree with the packet layout); two kinds of topic:
    STATE (latest value in a shared slot, with `seq` and `updated_at`; direct `ctx.out.<name>.publish(v)`; lossy
    on purpose; no read flag, each reader keeps its own `last_seen_seq`) and QUEUED LINK (a queue per reader, as
    before); NO sub-to-all: each module's config lists what it reads and writes, with compile time shorthands
    (groups, `all` minus its own outputs), and the macro rejects reading and writing the same topic, so there is
    no borrow conflict and every module can publish health (the outbox and commit idea was dropped again);
    `Topic` is an object safe trait (`apid`, `name`, `seq`, `updated_at`, `encode`, and `field_count`,
    `field_kind`, `field`) and a module reaches topics by APID through a sorted `(apid, &dyn Topic)` array the exec
    makes for it (`ctx.topic(apid)`); NO generic `S` (the array does the same; the field part is outlined, to be
    committed with table driven telemetry and FDIR); table rows are plain data by APID and field index with a
    dictionary hash in the header and are validated once at load (open: the table service's validate hook);
    `compare(Value, Op, Value) -> Result<bool, CompareError>` never panics (numbers exact via `i128`, NaN is an
    error, a mismatch is an error); rule violations are events, not topics; the recorder is a module (last in order,
    base rate, `all`, `seq` jumps reported); no mission wide types crate (a module that needs another's type
    depends on its crate; generic modules use `Topic`); commands also live in the module's own crate. The
    `exec!` example in design.md has `topics:` and an `fdir` module with `reads: all`. Open: field kinds
    supported by the derive (arrays and element indices, nested structs, text), stable topic ids across a
    config edit are now the user's APIDs, the table validate hook.

59. **Sample deployment S2c: the loop** (`sample-deployment/src/main.rs`; and `Clock::sleep_until_us` in the OSAL).
    `Clock` gained `fn sleep_until_us(&self, deadline_us)` (waits until the clock is at the deadline, returns at
    once if it already is; the one call meant to block, only the executive's loop makes it): `StdClock`
    sleeps for the remainder, `FakeClock` moves the time forward to the deadline (`fetch_max`, never back).
    3 new clock tests. In the sample: `TICK_US` = 10 ms (100 Hz); ticks start at multiples of it, so a tick has
    a number (`start_us / TICK_US`) and the `Exec` no longer keeps a counter (`tick(tick)` takes the number);
    `Exec::run(tick_limit)`: sleep to the boundary, tick, then the next start is `next_boundary_after(now)`, so an
    overrun SKIPS the boundaries it missed (nothing runs twice, a module due on a skipped tick does not step) and
    `ticks_skipped` is logged with `log::warn!` (an event later, when overruns are designed);
    `VEE_SAMPLE_TICKS` (invalid is `StartError::InvalidTickLimit`) ends the run through `Exec::stop`. 4 unit tests
    for the two pure functions. Checked by hand: 250 ticks take 2.5 s, the example steps at 1.0 s and 2.0 s, the
    second boot reports `prev end commanded`. The runs also showed `overrun` warnings: `Core::tick`'s blocking
    event log `append` (a `sync_data` per event) takes more than 10 ms now and then on WSL. design.md has the
    loop bullet in Executive Requirements. Tests now: 308 (core 71 + 6, core-api 69 + 7 + 9 + 10 + 4, core-types 32,
    osal 91 + 2 + 1, example 2, sample 4). The "WHERE WE LEFT OFF" and "Next" sections below were rewritten.

60. **Sample deployment S2d: the program, tested as a program** (`sample-deployment/tests/sample_runs.rs`, new, 4
    tests, about 1.3 s). Runs the real binary (`env!("CARGO_BIN_EXE_vee-sample-deployment")`) in a scratch
    directory, with `NOTIFY_SOCKET` and the other systemd variables removed, and checks what a person sees and what
    is left on disk: (1) two runs of `VEE_SAMPLE_TICKS=3`: the first says `reset 1: prev end unknown`, leaves
    two reset log records (boot, shutdown), the second says `reset 2: prev end commanded` (four records), and the
    event log (decoded with `vee_core_types::event_log`) has both runs' reset reports and the example module's two
    `started, previous end: ...` events; (2) 120 ticks: the example steps exactly once (`step 1 at ...`); (3) a run
    with no limit, killed (SIGKILL) once its boot record is on disk (polled, 10 s timeout): the reset log has just
    the boot, and the next boot says `prev end unknown, 1 unclean in a row`; (4) `VEE_SAMPLE_TICKS=soon`: non-zero
    exit, `InvalidTickLimit` on stderr, no files written. Mutation checked: making the sample stop with `Signal`
    fails test (1) only (reverted). The file has the usual `#![allow(clippy::unwrap_used, panic, indexing_slicing,
    arithmetic_side_effects, reason)]` and its own `TempDir`. Tests: 312 (sample 4 + 4). SLOC at this point (code
    lines, no blanks or comments, tests split off by `tests/` dirs and the first `#[cfg(test)]` of each file; the
    `fake_*` OSAL doubles count as non-test): 2291 non-test, 4208 test, 1215 comment lines in non-test code, 51 Rust
    files, 1490 Markdown lines (core 453, core-api 504, core-derive 129, core-types 209, example 39, osal 802, sample
    155; item 27 had 1140 non-test lines on 2026-11-18).

61. **Sample deployment S2e: the flight build, the guard, the unit files** (and a bug the guard found).
    (1) Root `Cargo.toml` `[profile.release]`: `panic = "abort"`, `lto = true`, `codegen-units = 1` (the sample's
    `cfg!(panic = "abort")` now installs the panic hook in this build). Measured on the sample: 579 KB without,
    468 KB with (376 KB stripped); `opt-level` s or z would save only 6% more, so it stays default; symbols kept.
    (2) `sample-deployment` feature `alloc-guard` (off by default): `main.rs` declares `GuardAlloc` as the
    `#[global_allocator]` under it; `start` already calls `arm()` unconditionally (a no-op without the guard).
    Run `cargo test -p vee-sample-deployment --features alloc-guard` (and `--all-features` clippy) in CI.
    THE GUARD FOUND A REAL RACE: the sample aborted after `arm()` because the system log's writer thread
    allocates in its OWN startup (std copies the thread name for the stack overflow handler), which happens
    after `spawn` returns, so after `arm()`. FIRST fix, REVERTED at the user's request ("a debug feature
    should be invisible to the running code"): a handshake in `std_syslog::start_queue`. FINAL fix, in the guard
    only: `std_alloc_guard::arm()` first sleeps `SETTLE` (100 ms) so threads started during startup have
    finished their own startup. A time based heuristic, accepted for a debug-only path: if it were ever too
    short it would fail loudly (an abort) in a guard build, never in flight. Checked: with a settle of 0, 8 of 8
    runs abort (exit 134); with 100 ms, runs and all 4 program tests pass with the guard armed (no allocation
    after startup in the loop, the module, the event log writes, the log writer and the shutdown). I found it
    with a temporary backtrace patch of the guard (reverted). `osal/src/linux/std_syslog.rs` is unchanged
    from the commit.
    (3) `sample-deployment/deploy/vee-sample.service` (default `Type=simple`, `StateDirectory`,
    `WorkingDirectory`, `LogsDirectory`, `StandardError=append:/var/log/vee/system.log`, `StandardOutput=null`,
    `WatchdogSec=5`, `Restart=on-failure`, start limit with `StartLimitAction=reboot-force` commented out) and
    `vee-sample.logrotate` (`size 10M`, `rotate 3`, `copytruncate`, `missingok`, `notifempty`). Validated with
    `systemd-analyze verify` and `logrotate -d`. (4) TESTED AGAINST A REAL SYSTEMD (this machine runs one; used
    `systemd-run --user`): a control that never sends `WATCHDOG=1` is killed after 1.1 s (result `watchdog`,
    SIGABRT); the release sample ran 4.0 s under `WatchdogSec=1` and finished `success`; the sample frozen with
    SIGSTOP at about 1 s was killed at 2.3 s (`watchdog`). So the keep-alives reach systemd, the implicit
    `NotifyAccess=main` lets them in, and a hung Vee is caught. NOT tested: the panic hook in the abort build
    (the hook's own test runs unwound), `Restart=` and `StartLimitAction` (config only). Tests: 312.

62. **File service F1: the shared vocabulary** (`core-api/src/file.rs`, new; `FixedStr::as_str` added). Decided with the
    user after a design discussion (the OSAL keeps ONLY OS implementations and the wrappers over them; the file
    service is the core's and owns its own error type, which is a superset of the OS's: `Busy`, `TimedOut`;
    modules see only core-api types; the dependency table in design.md does not change, osal still depends on
    nothing). `file::Error { NotFound, PermissionDenied, StorageFull, TooLarge, InvalidPath, Busy, TimedOut, Other }`
    (with `Display`, `core::error::Error`), `Tag { source: u8, number: u32 }` (the client's id, same space as an
    event's source, 0 is the core, and the client's own number), `Data` (inline `[u8; FILE_DATA_MAX_BYTES]` and a
    `len`, private fields so the bytes after `len` are zero and the derived `==` is right; `EMPTY`, `new`, `add_bytes`
    (all or nothing; named by the user over `push`, which read like `Queue::push`, `extend_from_slice` and `append`,
    which clashes with `Op::Append`: it builds ONE request's payload), `as_slice`, `len`, `is_empty`, `Default`; NOT `Copy`; `Debug` shows the length), `Path`
    (`FixedStr<FILE_PATH_MAX_BYTES>`) and `file::path(&str)` (strict: empty or non ASCII is `InvalidPath`, too long
    is `TooLarge`, NEVER cut: a cut path names another file), `Op { Append, WriteFile, ReadFile }` (the three
    things `Files` can do; `Delete` waits until `Files` can), `Request { tag, op }`, `Response { tag, result:
    Result<Data, Error> }` (`Ok` holds the bytes for a read, empty otherwise). Settings: `file::FILE_DATA_MAX_BYTES`
    (`VEE_FILE_DATA_MAX_BYTES`, 4096, 1..=65536) and `file::FILE_PATH_MAX_BYTES` (`VEE_FILE_PATH_MAX_BYTES`, 128,
    1..=1024); both in the sample's `.cargo/config.toml`. A request is about 4.2 KiB, so a queue of 8 is about 33 KiB
    (arithmetic, plus a test that a request is at most the data plus the path plus 64 bytes). QUEUE DEPTH (decided in the F1 discussion): depth = the sum of every client's max in flight requests, each
    enforced by a per client `Pending` table (a client at its cap gets `Busy` without touching the shared queue), so
    the queue cannot overflow by construction; caps today: event log 1, tables 2, modules as configured; the setting
    defaults to 8; `exec!` will compute and check the sum later. The `Service` trait
    (submit and poll) is NOT here yet: it comes with the code that needs it (F3/F5). 9 new tests in `file` and 1 in
    `fixed_str`; core-api: 79 + 7 + 9 + 10 + 4. The file service plan: F1 types (done), F2 OSAL channel and
    thread primitives, F3 the service in the core (a pure `handle(files, request)` and the worker thread), F4 the
    event log writer (removes the blocking `append` and the overruns), F5 `ctx.file`, routing, timeouts.

63. **File service F2: the OSAL's thread primitives** (`osal/src/channel.rs` and `osal/src/thread.rs`, new, TOP LEVEL
    concrete wrappers over std, file agnostic). Placement decided with the user: they were first written in
    `linux/` (as `std_channel`, `std_thread`) and MOVED to top level because they use only std (not Linux), nothing
    implements them twice, tests use real threads, and a trait would need generic associated types for the message
    type; a bare metal port would change these two files and could introduce a trait then. `lib.rs` documents them as
    the exception to "interface files use only `core`"; design.md (OSAL Requirements) says so too.
    `channel::channel::<T>(depth) -> (Sender<T>, Receiver<T>)` wraps `mpsc::sync_channel` (a depth of 0 becomes 1:
    for `mpsc` 0 means a rendezvous): `Sender::try_send(item) -> Result<(), T>` (full OR the receiver gone gives the
    item back, like `Queue::push`; not told apart), `Receiver::try_recv() -> Option<T>` (returns at once),
    `Receiver::recv() -> Option<T>` (WAITS; `None` once the sender is gone and the queue empty, which is how a worker
    learns to stop). One sender, not `Clone`. Items move by value: a test sends and receives a `[u8; 4096]` with ZERO
    allocations; `channel` allocates once. `thread::spawn(name, work) -> Result<(), SpawnError>` (`SpawnError` wraps
    the `io::Error`, with `Display` and `source`; the thread is not joined; never panics on a refused thread). No
    `recv_timeout` yet: it comes with the shutdown flush that needs it. 9 tests (7 channel, 2 thread; osal: 100 + 2
    + 1). Next: F3, the service in the core (using `vee_osal::channel` and `vee_osal::thread` directly).

64. **File service F3: the service in the core** (`core/src/file_service.rs`, new; `FakeFiles` made cloneable). `FileService::
    start(files: F: Files + Send + 'static) -> Result<FileService, thread::SpawnError>` makes the two queues
    (`FILE_QUEUE_DEPTH`, a core setting, `VEE_FILE_QUEUE_DEPTH`, default 8, 1..=256, in the sample config) and the
    worker thread `vee-file` (via `vee_osal::channel` and `thread`, used directly); `submit(request) ->
    Result<(), Request>` (full or worker gone: the request comes back; `#[allow(clippy::result_large_err)]` with a
    reason) and `poll() -> Option<Response>`, both non blocking. The worker: `recv` (waits), `handle`, `try_send` the
    response; a response that cannot be queued is dropped with `log::error!` (it cannot happen if clients keep to
    their limits); it ends when the service is dropped (the request queue closes). `pub fn handle(files, request) ->
    Response` is the pure part (Append, WriteFile, ReadFile through `Files`; a read uses a 4 KiB stack buffer;
    `error_from(FileError) -> file::Error`, a function and not a `From` impl because neither type is the core's
    own, orphan rule). QUEUE DEPTH RULE (module docs): depth >= sum over clients of each client's limit on
    outstanding requests; each client enforces its own (the `Pending` table, NOT written yet, F4/F5), so the
    queues cannot overflow; the request queue can physically hold one more (the worker holds one) but the response
    queue cannot, which a test of the full queue found (it overfilled the responses and the worker dropped one: the
    fix was a permit gate so the test takes each response before the next). `FakeFiles` is now `Clone` (an `Arc`
    inside: a clone is another handle to the SAME files) with `handles()` for tests (a first version of the
    worker-ends test was vacuous: it counted a fresh Arc; mutation checked: leaking the service now fails it).
    Tests: 8 unit (handle, the error mapping) and 6 in `core/tests/file_service.rs` (a request and its tag, an
    error and carrying on, order, a full queue hands the request back whole and nothing is lost, the worker ends on
    drop, submit and poll allocate nothing, with the guard as the test program's allocator); stable over 6 runs.
    design.md now has a "File Service" section (the seventh service had a table row but no section). A 7th thread test covers the worker's own `log::error!` (a response that does not fit): it submits two requests more
    than the response queue holds without polling, installs a capturing `log::Log` (one per test program; it looks only
    for its own tag `source: 9`), and checks that the drops are logged by tag, that every request was still DONE (the file
    has all the bytes), and that exactly the first `FILE_QUEUE_DEPTH` responses are there in order; mutation checked
    (changing the message fails it). Totals: 348 tests. Not used by anything yet. Next: F3b `Pending<N>` (the per client limit and timeouts), then
    F4 (the event log writer through the service; removes the blocking `append` and the sample's overruns).

65. **The system log uses the shared OSAL `channel` and `thread`** (`osal/src/linux/std_syslog.rs`; no duplicate queue
    and thread code any more). The user asked why the queue should ever close: in flight it never does (the global
    logger holds the sender for the whole process, and the writer thread runs for the same time), so the
    "is it closed" distinction was vestigial: it was used only by the `AlreadyStarted` race path (the stray writer
    ended on a closed queue) and one test. Changes: `Logger::queue` is `OnceLock<channel::Sender<LineBuf>>`;
    `start_queue` REFUSES a second start BEFORE it makes anything (`AlreadyStarted`), then `channel::channel` and
    `thread::spawn("vee-syslog", ..)` with a loop that never ends; `StartError::Thread` holds a `thread::SpawnError`;
    `drain` returns nothing (`while let` over `try_recv()`, which stays an `Option`: NO change to `channel`); the
    test of a closed queue is deleted. 347 tests. The user's larger idea (move the logger's POLICY to core, leaving
    the OS bits in the OSAL) is NOT done: obstacles found, to decide later: the global needs a set-once cell and
    `core` is no_std; the panic hook (in the OSAL) needs the `Line` and `LineBuf` format, so those stay in the OSAL;
    the direct mode fallback must survive; a generic logger cannot be a `static` until the concrete types are chosen
    (the deployment would own it).

66. **File service F3b: `Pending<N>`, a client's table of outstanding requests** (`core/src/file_client.rs`, new, 11 tests;
    358 tests in all). One per client (the event log writer, tables, each module's `ctx.file` handle), owned by the
    client, main thread only, no locks. It does three jobs: (1) MAKES THE TAGS (`begin(now_us, timeout_us) ->
    Result<Tag, Error>`: the client's `source` and a counting `number` that wraps; the request is recorded as
    outstanding with a deadline `now + timeout`, saturating), (2) HOLDS THE CLIENT TO ITS LIMIT `N` (`begin` is
    `Busy` when `N` are outstanding, and then the client must not submit; this is what makes the service's queue
    depth rule true; `N` of 0 allows none), (3) FINDS TIMEOUTS (`take_expired(now_us) -> Option<Tag>`, one overdue
    request per call, removed, overdue AT its deadline). `finish(tag) -> bool` takes a tag out: true if it was an
    outstanding request of this client (a response came, or the submit was refused and the slot is given back),
    false if not (late after a timeout, or another client's), and then the response is dropped. The usage, each
    tick: send = `begin` then `service.submit(..)` (refused: `finish`); collect = for each response, `if finish(tag)`
    use it else drop it; time out = `while let Some(tag) = take_expired(now)` answer `Error::TimedOut`. The timeout
    value is the client's (a setting comes with F4). Mutation checked (overdue one tick late fails 3 tests).
    Next: F4, the event log writer through the service (limit 1), which removes the blocking `append`.

67. **File service: ONE SERVICE PER CLIENT** (the user's decision, 2026-11-20: "one i/o is one file device"). `FileService::
    start(files, depth)` now takes its depth (the client's limit on outstanding requests = the `N` of its `Pending<N>`);
    the shared `FILE_QUEUE_DEPTH` setting and its sample config line are GONE, and so are the sum rule and the plan to
    route responses by `tag.source` (the field stays, for logs and tests). Costs, written in design.md: a thread per
    client (each blocked in `recv`), and no order between clients, so a file has ONE writing client. design.md's File
    Service section, the `Pending` docs and the `file_service` docs were rewritten; the tests pass a `DEPTH` of 8.
    Possible later: a `FileDevice<N>` that bundles a `FileService` and a `Pending<N>` (`request(now, timeout, op)` and
    `poll(now)`, which also synthesizes the `TimedOut` responses), which is what every client would otherwise write by
    hand; the case study of the FILE module below shows what a client needs from it.

68. **The FILE and CFDP modules, and a command rule** (design discussion, no code; design.md File Service section and
    Event Logger). FILE = the operator's handle on the filesystem only (list, mkdir, delete, rename or move, copy, size
    and checksum; results go out as a typed topic through the link module; path allow and deny lists from the
    deployment, with the core's reset log and event log on the deny list); CFDP = whole file transfer to and from
    ground in chunks; table upload is CFDP + (FILE) + the core's table command. Both are MODULES (touch no core state,
    mission policy, removable attack surface, cFS precedent: its File Manager is an app). The user's rule for a
    command module when its device is full: NAK `Busy` immediately, do NOT park the command (no waiting queue), so
    executed commands run in arrival order; the earlier idea of a waiting queue and of "pop only when there is
    room" were dropped for a module with mixed commands (a slow disk must not hold up its other commands).
    `FileDevice<N>` therefore needs `request` (`Busy` is the normal "full" answer, handled per client: the event log
    writer keeps its batch and retries, FILE NAKs) and `poll`, and NO `has_room`. NOT written: the new ops
    (`Delete`, `Rename`, `Copy`, `List`, `Mkdir`, `Crc`, `ReadAt`, `WriteAt` or an offset append) and the OSAL `Files`
    methods behind them.

69. **File service F3c: `FileDevice<N>`, and a bug it found** (`core/src/file_device.rs`, new; `core/tests/file_device.rs`,
    7 tests; shared test helpers moved to `core/tests/common/mod.rs`: `Gate` and `GatedFiles`). `FileDevice<N>` =
    `FileService` (depth `N`) + `Pending<N>`: `start(files, source)`, `request(now_us, timeout_us, op) ->
    Result<Tag, Error>` (`begin`, then `submit`; a refused submit gives the slot back and answers `Busy`),
    `poll(now_us) -> Option<Response>` (a real response first, so an answer in the same step as the deadline wins; a
    response whose tag is no longer pending is DROPPED with a `log::debug!`; then `TimedOut` for one overdue),
    `outstanding()`. No `has_room`. THE BUG: with the response queue the same depth as the request queue (`N`), a
    stale request (timed out, still in the worker) and a fresh one could both finish before the client's next poll,
    and the second response was dropped, so the client reported TimedOut for an operation that worked. FIX: the
    response queue has `depth + 1` slots (the worker holds one request while the queue holds `depth`); mutation
    checked (without it 3 tests fail). A test of the full capacity was added to `file_service.rs`, and the "does not
    fit" test now overflows `DEPTH + 1`. design.md: the queue depth bullet rewritten, and the decisions recorded:
    chunked reads are client driven with `ReadAt { path, offset, len }` (NOT written; a short or empty chunk is the
    end), a file being transferred is not written, most clients use `N` of 1. 366 tests. Next: F4, the event log
    writer on a `FileDevice<1>`.

70. **The event system and the worker, designed (no code)** (user discussions 2026-11-20; design.md Events, Event Logger and File
    Service updated). (1) Events have TWO consumers, each with its own queue: `Ev` stays a STAGING buffer emptied every
    tick, in order, by the core, which feeds (a) the TELEMETRY queue (a queued bus link, writer the core, reader the link
    module; the severity filter applied BEFORE the push) and (b) the event LOG worker's channel (everything). Each sink has its
    own depth and drop count; the per sink "dropped" event is not written. (2) The event log has a DEDICATED worker with an
    `Event` typed channel (about 80 bytes a slot), NOT the generic file service (a 4.2 KiB slot per 78 byte event, and one
    request in flight would be one event per tick); it appends each event with `event_log::append`, one sync per record
    (kept per record, NO group commit for now: about 2 ms a record measured, so 400 to 500 a second); the main side
    `try_send`s and counts and reports a failed send as an event; the worker logs failed writes. (3) A generic
    `Worker<Req, Resp>` in the core (message TYPES, not byte sizes; a handler closure; `submit`, `poll`, `finish(timeout_us)`)
    underlies both the file service and the log writer, and can serve any blocking I/O. `finish` is the shutdown signal with a
    timeout, which needs `channel::Receiver::recv_timeout` in the OSAL (only the platform can wait with a timeout; the core
    has no clock to sleep with). (4) The earlier plan of F4 on a `FileDevice<1>` is RETRACTED. (5) Also decided: the strict
    one-in-flight model (a timed out request keeps its slot until the worker really finishes; the response queue back to
    `N` slots, dropping the `+1`) is the user's preference; NOT yet applied to the code (it still has the `depth + 1`
    response queue and frees a slot at the timeout). NOT written: `recv_timeout`, the `Worker`, the log writer, the
    telemetry queue, the per sink drop events.

71. **The event system: design G chosen, item 70 SUPERSEDED** (user decision, 2026-11-20, after a review of the alternatives:
    staging with tick fan-out and a dedicated log worker (A), a shared ring with cursors (B, needs unsafe), the log as a bus
    module (C, later, needs the bus and a module stop hook), a main thread write with no sync (D, a `write()` can block
    under writeback throttling), an mmap'd ring (E, unsafe), and dropping the binary log in favour of the system log
    file (F, loses typed records, CRC and independence from stderr redirection)). DECIDED: (G) `Ev` holds TWO plain main
    thread queues filled at send time, the LOG queue (everything, the burst absorber) and the TELEMETRY queue (the
    downlink severity filter applied first), each with its own depth and drop count (a full queue drops the new event
    and counts it). The event log writer is a client of the existing file service (`FileDevice<1>`, strict one in flight):
    when its service is idle it builds ONE batch by peeking at the log queue, encoding the record and `Data::add_bytes`
    (the first record that does not fit ends the batch, so NO fixed 52: the size is `FILE_DATA_MAX_BYTES`), pops an event
    only after `add_bytes` took it, submits one `Append`, and collects the response on a later tick. One sync per batch
    (group commit by tick: at low rates one event per batch; the durability given up is about one tick). NEEDED, not
    written: `Queue::peek` (core-api), `Ev` with two queues and drop counts, a compile time check that the record fits an
    empty `Data`, the strict one in flight change (a timed out request keeps its slot until the worker finishes; the
    response queue back to `N`, dropping the `+1`), `Receiver::recv_timeout` in the OSAL and a flush for `Core::stop`,
    the writer in `Core::tick` replacing the blocking `append`, the per queue drop events, a log timeout setting. The generic
    `Worker<Req, Resp>` is DEFERRED until a second blocking use exists. design.md Events, Event Logger and File Service
    updated.

72. **`N` and tags DROPPED, designed only (the code still has them)** (user decisions, 2026-11-20; design.md File Service updated).
    EVERY file service is strictly ONE request in flight: one slot in each queue, no depth parameter, no `Pending<N>`
    (the 11 tests of `file_client.rs` go with it), no `Tag` (a response is always the answer to the one outstanding
    request), a `FileDevice` with a three state value (idle, waiting with a deadline, timed out and still waiting for the
    worker), `request(now, timeout, op) -> Result<(), Busy>`, `poll(now) -> Option<Result<Data, Error>>`. A timed out
    request is reported `TimedOut` but the device stays BUSY until the worker finishes; a late response is dropped.
    More streams = more devices. CFDP: ONE device to start (a serial transaction does one chunk at a time in either
    direction; a second device only for concurrent transactions or read-ahead; a faster link would first want a larger
    `FILE_DATA_MAX_BYTES`). My earlier example of one device for reads and one for writes was wrong. NOT yet applied to
    the code (`FileService::start(files, depth)` with `depth + 1` responses, `Pending<N>`, `Tag`, `FileDevice<N>` still
    exist), to be done in the step after the two queue `Ev`.

73. **`Queue::peek` and `Queue::take_refused`, and the two queue `Ev`** (design G, first code step; 383 tests, was 366).
    `core-api/src/queue.rs`: `peek(&self) -> Option<&T>` (the oldest item without taking it, for a consumer that must see
    an item before it knows it fits) and a REFUSAL COUNT inside `Queue` itself (`push` counts every refusal, whatever the
    caller then does; `take_refused()` returns and resets it): so no event specific wrapper type was needed (the user
    asked to justify an `EventQueue<N>` and it had no event specific method, only the depth `N`), and the bus's queued
    links can reuse it. `core-api/src/ev.rs`: `Ev` has PUBLIC fields `log: Queue<Event, EV_LOG_QUEUE_DEPTH>` (1024, was
    `EV_QUEUE_DEPTH`, renamed everywhere incl. the sample config) and `telemetry: Queue<Event, EV_TELEMETRY_QUEUE_DEPTH>`
    (256, new), a private `telemetry_min: Option<Severity>` and `set_telemetry_filter(Option<Severity>)` (OFF by default,
    so the queue cannot fill with nobody to read it); `send` pushes to `log` always and to `telemetry` when the severity
    is at least the filter; the old `pop`, `len`, `is_empty`, `take_dropped` are gone (callers say `ev.log.pop()`,
    `ev.log.take_refused()` and so on). `Severity` now derives `PartialOrd, Ord` (Info < Warn < Err). `events::
    report_dropped` sends one `EventsDropped` per queue that refused events: `N log events dropped: the queue was full`
    or `N telemetry events dropped: ...` (at most 55 bytes). New tests: 3 for `peek`, 3 for the refusal count, 9 for the
    two queues in `core-api/tests/ev.rs` (off by default, the filter, `Info` sends all, `Err` only the worst, the filter
    turned off, a full telemetry queue leaves the log alone and the other way round, `peek`), 2 in `events.rs`. `Core::tick`
    still writes the log queue with the BLOCKING `append` (so the sample still shows overruns); the telemetry queue has no
    consumer yet. Next: apply the dropped `N`/tags change (item 72), `recv_timeout` and the flush, then the log writer.

74. **The telemetry queue removed, the file service made strictly one-in-flight, and the event log writer written** (user
    decisions 2026-11-20 and 2026-11-21; design.md Events and File Service updated). Item 73's telemetry parts are SUPERSEDED: `Ev` has
    only `log`; `EV_TELEMETRY_QUEUE_DEPTH`, `telemetry_min` and `set_telemetry_filter` are gone; telemetry is future bus work.
    Item 72 is now APPLIED to the code: no `N`, no `Tag`, no `Request`, `file_client.rs` deleted; `file::Response =
    Result<Data, Error>`; `FileService` (one slot each way: `start`, `submit`, `poll`, `wait`, `handle`); `FileDevice` (state Idle /
    Waiting{deadline} / TimedOut: `request`, `poll`, `wait`, `busy`; mutation checked: freeing the device at the timeout fails the
    "stays busy" test); OSAL `Receiver::recv_timeout`; `Platform::Files: Clone + Send + 'static`. New `core/src/log_writer.rs`
    (`LogWriter::start/step/flush`, setting `EVENT_LOG_TIMEOUT_US`, compile-time check that a record fits `FILE_DATA_MAX_BYTES`);
    `event_log::batch(&mut Ev) -> file::Data` replaces `event_log::append`; `Core` has `log_writer: Option<..>` started in
    `Core::start` (`EventLogUnavailable` if it cannot start), `tick` calls `writer.step`, `stop` flushes, then reports dropped, then
    flushes again, and `Core::flush_event_log` is public for tests that read the log. The blocking write is gone from `tick`.
    Sample run (8 s, release): 2 overruns where it had one per event; the cause of those two is not yet found. 369 tests; clippy
    and fmt clean. Not done: the `Core::tick` allocation-free assertion, `write_file` still uses `format!` (allocates on the worker).

75. **The sample's overruns explained** (2026-11-23; `sample-deployment/src/main.rs`). The 2 overruns left after
    item 74 are NOT Vee's work: a probe in `Exec::run` showed the loop woke 12.8 ms late (work took 2 us). A plain
    `thread::sleep` loop with no Vee code on WSL2 also wakes 2 to 11 ms late about once per 10 s. The user ran the
    sample on native Linux: no overruns. The cause is WSL2's scheduler and timer, not the code. `Exec::run` now
    reads the clock right after the sleep and the overrun `log::warn!` says `woke N us late and its work took M us`,
    so a late wake-up and a slow tick can be told apart. The skip rule is unchanged (it is about clock time, whatever
    the cause). Decided with the user: do this now, without waiting for the overrun event; the Health domain will
    take the same two numbers when overrun detection is designed (a late wake-up and a slow tick may get different
    severities). Not done: `clock_nanosleep` with an absolute time (needs `unsafe`, and would not fix WSL).
    Tests unchanged: 4 sample unit + 4 sample run tests pass, clippy and fmt clean.

76. **Software bus design split out and settled** (2026-11-23; `docs/bus.md` new and complete for what is decided,
    `docs/design.md` "Software Bus" is a pointer, its Events bullet on telemetry updated). No code. Decided with the
    user: (a) use cases enumerated (telemetry state and queues by table, FDIR, pipelines, recorder, fan-in, flow
    control); table driven modules are wired to every topic they could need at compile time and filter inside the
    module, nothing subscribes at runtime; (b) typed handles for ordinary modules (`Module::In<'a>` and `Out<'a>`,
    `ctx.inputs` and `ctx.out`, compile time errors) plus a dyn view by APID for telemetry, FDIR and the recorder,
    both built from the same `Topic` (option Y; the F Prime split is the same); (c) state topic: one shared slot, no
    copy per reader, `latest() -> Option<&Sample<T>>`; (d) queued topics: a queue per reader (fan-out), policy per
    queue (refuse or drop and count), `publish` returns `Result<(), Full>`, flow control by `writer.space()` (a
    credit count, no blocking), fan-in by one queue topic per producer; (e) a reader that runs before its writer is
    a WARNING in the macro; (f) field access is required, done as DATA (2026-11-23, later in the day): the derive writes `encode` and a
    `LAYOUT` (name, wire offset, kind per field) for every message type, one hand written `read(bytes, field)`
    in safe Rust reads a field (no cast, no `unsafe`), rows name a field by index, queued topics have a layout but
    no "latest"; the dyn view (`&dyn Topic`, implemented once generically) is the erased collection a table driven
    module needs, since its topics have different types and the table gives APIDs as data (later than slice one); (g) core topics are normal topics; APIDs are unique and defined in ONE place, the deployment's topic list (never in a module crate), a duplicate is a compile error (no range check on the `u16` for now); (h) events: one `Ev`, `send` also
    pushes to a second queue for the bus only if a severity filter allows (Off is an option and the default),
    switched by a CORE command (reaching the core through a command sink fed by a link module), `Ev` has its own
    step at the top of `core.tick` that flushes to the events topic, one tick of delay accepted. First slice: typed
    state topics only, hand wired in the sample. Open: how a queued writer reaches its N queues, fan-in fairness,
    derive field kinds and `Value`, the bus queue's depth and the filter command's shape. Also this session: the
    sample's overrun warning now says how late the loop woke and how long the work took (item 75).

77. **Bus slice 1, step 1: `bus::StateTopic<T>`** (`core-api/src/bus.rs`, new; `pub mod bus` in `lib.rs`). `Sample<T> {
    value, seq, updated_at }` (derives `Debug, Clone, Copy, PartialEq, Eq`; `T` is not bounded) and `StateTopic<T>`:
    `new()` (a `const fn`), `publish(now, value)`, `latest() -> Option<&Sample<T>>`, `Default`. No value until
    the first publish (`None`, no default value). `seq` is 1 for the first publish and WRAPS at `u32::MAX` (a
    counter that stuck would look like "no new value" for ever). The topic is one slot; there is no copy per
    reader. 6 tests (core-api: 75 lib tests). Not yet: `StateWriter` and `StateReader` (the handles a module gets;
    a module never holds the topic), `Module::In`/`Out`, `ctx.inputs`/`ctx.out`, the loop test and the sample's
    second module (the rest of slice 1, in `docs/bus.md`).

78. **Bus slice 1, step 2: the handles** (`core-api/src/bus.rs`). `StateTopic::writer(&mut self, now) ->
    StateWriter<'_, T>` and `StateTopic::reader(&self) -> StateReader<'_, T>`. `StateWriter` holds `&mut StateTopic`
    and the time of the call (private fields); its one method is `publish(value)`, which uses that time, so every
    publish in a call has the same time. `StateReader` holds `&StateTopic`; its one method is `latest() ->
    Option<&'a Sample<T>>` (the reference lives as long as the topic borrow `'a`, not only as long as the
    handle). No derives on the handles. Checked by hand (a temporary test file, deleted): a reader has no
    `publish`, a writer has no `latest`, a reader held across a write does not compile (E0502), and a reader
    of another type does not fit. 4 new tests (core-api: 10 in `bus`; workspace 379). Not yet: `Module::In<'a>`
    and `Out<'a>`, `ctx.inputs` and `ctx.out`, the loop test and the sample's second module.

79. **Bus slice 1, step 3: `Module::In`/`Out` and `ctx.inputs`/`ctx.out`**. `Module` has two generic associated types,
    `type In<'a>;` and `type Out<'a>;` (no defaults: a module with no bus handles writes `type In<'a> = ();` and
    `type Out<'a> = ();`). `Ctx<'a, M>` has `pub inputs: M::In<'a>` and `pub out: M::Out<'a>`; `Ctx::new` takes both
    (7 arguments). `Slot::init` and `Slot::step` take `inputs` and `out` too, with ONE lifetime `'a` for `&'a mut
    self`, `&'a mut Ev`, `inputs` and `out`: the compiler cannot shorten `M::In<'a>` (it does not know how a module's
    type uses `'a`), so the simple signature fails ("lifetime may not live long enough", tried by hand). Every
    existing module and call now says `()` and `(), ()` (the example module, the three loop test modules, the two
    ctx test modules and their `run_init`/`run_step` helpers, the sample's `Exec`). No behaviour change: 379 tests
    pass, clippy and fmt clean. Not yet: a module that actually uses a topic. Next: the loop test with two modules
    (one publishes, one reads), including the order rule, then the sample's second module.

80. **Bus slice 1, step 4: the loop test** (`core/tests/bus_loop.rs`, new, 4 tests; workspace 383). A hand written
    executive owns one `StateTopic<u32>` and two modules, `Sensor` (`Out` = `SensorOut { reading: StateWriter }`,
    publishes 10, 20, 30 ... and sends an event) and `Controller` (`In` = `ControllerIn { reading: StateReader }`,
    reports `value N seq S updated T` or `no reading` as an event). `Exec` makes the handles for each call with a
    struct literal (`self.reading.writer(now)`, `self.reading.reader()`) and passes them to `Slot::step`; `Order`
    picks which module is called first. Tests: a reader after its writer sees this tick's value; a reader before
    it sees the last tick's (`no reading` in tick 0); a slow reader sees only the latest and `seq` jumps (1 to 5);
    a fast reader sees the same sample and `seq` until the writer publishes again. Mutations checked: swapping the
    call order fails the three order tests, a `seq` that does not increase fails all four. FOUND: the exec reads
    the clock for a writer's `now` and `Slot::step` reads it again for `Ctx`, so the two can differ with a real
    clock (the fake does not move inside a call): added to the open list in `bus.md`. Next: the sample
    deployment's second module that reads the example module's topic.

81. **One clock read per call** (`core/src/slot.rs`). `Slot::init` and `Slot::step` now take `time: u64` (the
    monotonic time the executive read just before the call) instead of `clock: &impl Clock`, so the time in a
    module's `Ctx` and the time the executive gave a writer's handle (`StateTopic::writer(now)`) are the same
    number by construction. `slot.rs` no longer imports the OSAL clock. Callers read the clock inline per call
    (`self.clock.monotonic_us()`): the sample's `Exec::tick` and `start`, `exec_loop.rs`; `bus_loop.rs` reads `now`
    once per call and uses it for both. The open item in `bus.md` became a settled bullet. No behaviour change:
    383 tests pass, clippy and fmt clean. Next: the sample deployment's second module that reads the example
    module's topic.

82. **Bus slice 1, step 5: the sample's second module. SLICE 1 IS DONE.** `vee-example` now publishes a state topic:
    `Progress { steps }` (declared in that crate), `ExampleOut { progress: StateWriter<Progress> }` as
    `Module::Out`, and `step` publishes after sending its event; 2 new tests (4). New crate `modules/watcher`
    (`vee-watcher`, depends on `vee-core-api` and `vee-example` for the type; in the workspace members and the
    sample's dependencies): `Watcher` has `In` = `WatcherIn { progress: StateReader<Progress> }`, keeps
    `last_seen: Option<u32>` (the `seq` it last reported; `Option`, not 0, because `seq` wraps), and on a new `seq`
    sends `Saw` (Info): `example at step N, seq S, M missed, A us old` (`M` = publishes replaced before it looked,
    `A` = `ctx.monotonic() - updated_at`); nothing for no value or a value already seen. 5 tests. The sample's
    `Exec` owns `progress: StateTopic<example::Progress>` and two slots; `tick` is core report, core tick, then
    `step_example` (builds the writer) and `step_watcher` (builds the reader), in that order; `start` makes the
    topic, calls both `init`s with their handles, and moves it into `Exec`. The watcher steps every 200 ticks
    (`WATCHER_PERIOD_TICKS`), the example every 100, so the watcher misses every other value. By hand: `example
    at step 2, seq 2, 1 missed, 19 us old` at 2 s, `step 4, seq 4, 1 missed` at 4 s. New program test in
    `sample_runs.rs` (450 ticks, 4.6 s): checks the invariants (step equals seq, one missed after the first look)
    and not the tick numbers, because those depend on how long startup took. One run of the old version of
    that test (exact numbers) failed once under `--features alloc-guard` and passed 20 times after; the cause
    was not found (startup time or a skipped tick are the suspects), so the test was made less exact. 391 tests,
    clippy and fmt clean. NEXT (in `docs/bus.md`): queued topics (a queue per reader, policy, `space()`), then
    `Ev`'s bus queue and flush, the `bus::Message` derive with a layout, APIDs and the dyn view.

83. **Queued topics, step 1: storage (`QueuedTopic<T, N, D>`), no handles yet** (`core-api/src/bus.rs`). The open
    question in `docs/bus.md` ("how does a queued topic's writer reach its N queues") was settled with the user
    first: ALL readers of one topic share one depth `D` (`[queue::Queue<T, D>; N]`), not a depth per reader —
    simpler, no macro needed, matches `Queue<T, N>`'s own const-generic shape; a reader that genuinely needs a
    different depth gets its own topic. `Policy { Refuse, Drop }` is chosen PER READER, not per topic: `Refuse`
    makes `publish` report `Err(Full)` when that reader's queue is full (a flow controlled link checks `space()`
    first and should rarely see it); `Drop` lets a full queue silently drop the new message, counted by the queue
    itself (`Queue::take_refused`, reused rather than a second counter). `publish(value) -> Result<(), Full>`
    pushes a COPY (`T: Copy`, the first bound this crate puts on a bus message type) into every queue that has
    room; one full queue never stops delivery to the others (checked by a test: a `Refuse` reader stays full and
    keeps its old value while a `Drop` reader that had freed up still gets the new one). `pop(reader)`,
    `len(reader)`, `is_empty(reader)`, `take_refused(reader)` (a plain `usize` index for now: the typed per-reader
    handle is the next step) and `space()` (the smallest free count among all readers' queues; `D` if there are
    none). No `Sample`, `seq` or `updated_at`: queued messages are not timestamped by the topic (unlike a state
    topic's `Sample`). 8 new tests (core-api lib: 102; workspace 399). clippy and fmt clean. `docs/bus.md` updated
    (the open question is settled, a new "Queued topics, step 1" section). Not yet: `QueueWriter`/`QueueReader`
    handles (so a module's `ctx.out`/`ctx.inputs` hold a specific reader index, not a raw `usize`),
    `Module::In`/`Out` wiring for a queued topic, a loop test (like `core/tests/bus_loop.rs` for state topics),
    `Ev`'s bus queue.

84. **Queued topics, step 2: `QueueWriter`/`QueueReader` handles** (`core-api/src/bus.rs`). The queued
    counterpart of `StateWriter`/`StateReader`. `QueuedTopic::writer(&mut self) -> QueueWriter<'_, T, N, D>`
    (no `now`: queued messages carry no `Sample`/timestamp, unlike a state topic) and
    `QueuedTopic::reader(&mut self, reader: usize) -> QueueReader<'_, T, N, D>` (`reader` is this handle's index
    among the `N` queues, fixed when the handles are wired). ONE DIFFERENCE from the state topic worth noting: a
    queued reader MUTATES (popping removes the message from its own queue), so `QueueReader` borrows the WHOLE
    topic mutably, not immutably like `StateReader` does; this costs nothing in practice because, same as the
    state topic's handles, each module's handle is made fresh for one call and dropped when it returns, so a
    writer's and a reader's borrows never overlap. `QueueWriter` has `publish` and `space()`; `QueueReader` has
    only `pop()` (no `take_refused`: that stays on `QueuedTopic` itself, reached directly by whichever core-level
    code owns the topic, the same way `Ev`'s log queue is a `pub` field its consumer uses directly). Checked by
    hand (temporary code added then deleted, per item 78's method): a writer has no `pop` (E0599), a reader has
    no `publish` (E0599), a reader held across a later `topic.writer()` call does not compile (E0499, two
    `&mut` borrows of the topic at once), and a reader handle typed for the wrong message type does not fit
    (E0308). 4 new tests (core-api lib: 106; workspace 403). clippy and fmt clean. `docs/bus.md` updated. Not
    yet: `Module::In`/`Out` wiring for a queued topic, a loop test (like `core/tests/bus_loop.rs`), `Ev`'s bus
    queue.

85. **Queued topics, step 3: `Module::In`/`Out` wiring and a loop test** (`core/tests/queued_bus_loop.rs`, new).
    A hand written executive owns one `QueuedTopic<u32, 2, 2>` (reader 0 `Refuse`, reader 1 `Drop`) and three
    modules: `Producer` (`Out` = `QueueWriter`) and `Reader` (`In` = `QueueReader`) — ONE `Reader` type used for
    BOTH readers, since a reader's behaviour does not depend on which queue or policy the executive wired it to,
    only the index does (an instance of "one common implementation over two" from `CLAUDE.md`). 3 tests: fan-out
    (both readers get their own copy of every message, in order), a FIFO backlog up to the queue's depth for a
    reader that falls behind (oldest first, exact values checked), and that only the `Refuse` reader's fullness
    makes the writer see `Err(Full)` for that publish (a `Drop` reader's fullness never blocks the writer; its
    loss is silent to it, found instead with `exec.feed.take_refused(1)`, no module involved, matching item 84's
    design).
    FOUND while writing it, a gap in the test harness (not in `QueuedTopic`): this file's executive, like
    `core/tests/bus_loop.rs` before it, never called `Slot::report_suppressed`, which is also what starts a
    module's new tick of throttle counting (`core-api/src/throttle.rs`, item 25). Skipping it makes
    `EV_MAX_PER_TICK` (default 5) a budget for the WHOLE TEST RUN instead of per tick: a code sent every tick
    for more than 5 ticks (this file's `Producer`/`Reader`; `bus_loop.rs`'s tests never happened to run that
    many ticks of the same code) is silently dropped by the throttle from the 6th occurrence on, with no error,
    only a missing line in the event log. Spent real debugging time on this: first suspected the async file
    service worker thread (a real OS thread even under `FakeFiles`), confirmed it was NOT that by instrumenting
    `log_writer.rs`/`file_device.rs` with temporary `log::debug!` lines (reverted; `git status` confirms both
    files are unchanged) and running with `VEE_TEST_LOG=debug`, which showed the module's `step` being called
    but no event log line at all for it, which is what throttling (not a lost write) looks like. FIXED in
    `queued_bus_loop.rs`'s `run_tick`: calls `report_suppressed` for every slot before `core.tick`, matching the
    documented loop order (`docs/design.md`, Executive Requirements). `bus_loop.rs` has the SAME gap, left alone
    (out of scope here; its own tests pass only because none of them send one code more than 5 times across the
    whole run). `docs/bus.md` updated ("Queued topics, step 3"). Workspace: 406 tests (was 403). clippy and fmt
    clean.

86. **Events on the bus, step 1: `Ev`'s bus queue and flush** (`core-api/src/ev.rs`). `Ev` gets a second queue,
    `bus` (new setting `EV_BUS_QUEUE_DEPTH`, default 64), beside the existing `log` queue, and a severity floor
    filter (`Option<event::Severity>`, `None`/Off by default, set only through `set_filter`, no module holds
    it). `send` now also pushes into `bus` when the event's severity meets the floor; a full `bus` queue
    refuses and counts independently of `log`. `flush_bus<const N, const D>(&mut self, topic: &mut
    bus::QueuedTopic<Event, N, D>)` drains `bus` into a queued topic, oldest first, ignoring a `Refuse`
    reader's `Err(Full)` the same way `send` ignores a full `log` (the loss is already counted on that
    reader's own queue). NOT wired into `Core::tick` and no loop test yet: nothing in the workspace reads an
    events topic today, so there is no `N`/`D` to size one with; tested standalone instead (5 new tests:
    off by default, floor lets through that severity and worse, a full bus queue's refusal count is separate
    from `log`'s, flush publishes in order and empties the queue, flush is not stopped by a refusing reader).
    `docs/bus.md` updated ("Events on the bus, step 1"). Workspace: 411 tests (was 406). clippy and fmt clean.

87. **The `bus::Message` derive, with field access** (`core-api/src/message.rs` new, `core-derive/src/lib.rs`,
    `core-api/src/bus.rs` re-exports it). `message.rs` has the hand written, type-agnostic half: `Kind` (the
    11 wire kinds this step supports: `Bool`, `U8`..`U64`, `I8`..`I64`, `F32`, `F64` — nested structs, arrays
    and text are explicitly open, see `docs/bus.md`), `Field` (name, wire offset, kind) and `read(bytes,
    field) -> Option<Value>` (`Value` is `Int(i128)`/`Float(f64)`/`Bool(bool)`, widened so a table row's limit
    need not match the field's exact width or signedness; a bad offset or short input is `None`, never a
    panic). `#[derive(bus::Message)]` (`core-derive`, `derive_message`, same shape as `derive_code`) writes
    the type-specific half on a plain named-field struct: `const WIRE_LEN: usize`, `const LAYOUT: &'static
    [message::Field]`, and `fn encode(&self) -> [u8; WIRE_LEN]` (big endian, declaration order, no padding —
    the WIRE layout, never the in-memory struct's). A field whose type is not one of the 11 kinds (a nested
    struct, an array, a `String`) is a clear compile error naming the field. These are inherent methods/consts
    on the struct, NOT a trait: nothing needs one yet (the dyn view, which does, is a later roadmap item), so
    there is no `Message` trait, only the derive of that name. Tested in `core-api/src/message.rs` (5 unit
    tests: each integer kind big endian, floats and bool, a nonzero byte is still `true`, a short or
    out-of-range read is `None` not a panic, an offset near `usize::MAX` does not overflow) and
    `core-api/tests/message_derive.rs` (7 tests, outside the crate because the derive's `::vee_core_api` paths
    do not resolve inside it, same reason as `tests/code_derive.rs`: `WIRE_LEN`, `LAYOUT`'s offsets and kinds,
    `encode`'s bytes, a `false` bool encodes to 0, a round trip through `read` by `LAYOUT`, a message with no
    fields, and that a derived message type can be a `QueuedTopic`'s `T`). Hand checked (temporary, reverted):
    a field of an unsupported type (`alloc::string::String`) is rejected with the derive's own error message,
    not a confusing one from further down the generated code. `docs/bus.md` updated ("The `bus::Message`
    derive, with field access"). Workspace: 423 tests (was 411). clippy and fmt clean.
    Split right after, on request: `core-derive/src/lib.rs` now only has the two `#[proc_macro_derive]`
    entry points (rustc requires those at the crate root) and their doc comments; each derive's own parsing
    and codegen moved to its own file, `code.rs` (was already there, pulled out) and `message.rs`. No
    behaviour change: same 423 tests.

88. **Renamed APID to TID** (user, 2026-11-24): APID is a CCSDS downlink concept with its own constraints (11
    bits, 0x7FF reserved); the bus's own topic identity does not need to carry that, so it is `bus::Tid = u16`
    everywhere, and a deployment that downlinks CCSDS chooses its own TID-to-APID mapping later. Renamed
    throughout `docs/bus.md` and `docs/design.md`; left untouched in dated log entries here and in
    `latest-chat.md` (historical record of what was said at the time, not current-state docs).

89. **Split `core-derive` into one file per derive** (`core-derive/src/code.rs`, `core-derive/src/message.rs`,
    on request). `lib.rs` now only has the two `#[proc_macro_derive]` entry points and their doc comments
    (rustc requires those functions at the crate root); each derive's own parsing and codegen moved out
    unchanged. No behaviour change.

90. **TIDs and the dyn view, step 1** (`core-api/src/bus.rs`, `core-api/src/message.rs`, `core-derive`). The
    `Message` derive now ALSO writes `impl message::Message for` the struct (same `WIRE_LEN`/`LAYOUT`, plus
    `encode_into(&self, out: &mut [u8]) -> Option<usize>`, since a generic `[u8; Self::WIRE_LEN]` return is
    not something a trait method can do on stable Rust) — the first thing in this bus work that needed a
    TRAIT, not just inherent items, because `impl<T: message::Message> Topic for StateTopic<T>` is generic
    over every message type. `bus::Topic`: `layout()`, `latest_bytes(out) -> Option<usize>` (raw bytes, for
    the recorder), `pop_encoded(reader, out) -> Option<usize>`, implemented once for `StateTopic<T>` and once
    for `QueuedTopic<T, N, D>`; each leaves the other side's methods at the trait default (`None`), since a
    state topic has no queue and a queued topic has no "latest". `bus::find(topics, tid)`: a binary search by
    `Tid` over a `(Tid, &dyn Topic)` list, written once. New setting `MESSAGE_MAX_BYTES` (default 3072,
    `core-api/src/message.rs`): the "Settled" cap ("a message is Copy plain data, up to about 3 kB") finally
    has a concrete value, used as the stack buffer a default method encodes into.
    SETTLED while building this, from a question the user asked about the previous step: nothing stops
    `message::read(bytes, field)` from being called with a field and bytes that do not belong together — it
    is a safe (bounds-checked, no panic) but fully trusting primitive, like `core-types/src/record.rs`'s
    `field()`. Asked the user (AskUserQuestion) whether `Topic` should close that gap structurally for the
    dyn view or stay as originally sketched and rely on caller discipline; chose to close it: `read_latest`
    (NEW, not in the original sketch) is a DEFAULT method built on `latest_bytes` + `layout` of the SAME
    topic, so a caller supplies only a plain index (validated against `layout().len()` once, at table load,
    not written yet), never a raw `Field`. This only closes the gap for the "latest value" (state) side;
    `pop_encoded` + `message::read` by index is still the queued-topic path, left for when telemetry/FDIR
    actually evaluate table rows against queued topics (one pop can be followed by several field reads of
    the same popped message, which one bundled call cannot express).
    NOT done: a TID uniqueness check (nothing to check yet: only one real topic exists today, `Example`'s
    `progress`) and the `(tid, &dyn Topic)` array and table themselves (`exec!`'s job). `docs/bus.md` updated
    ("TIDs and the dyn view, step 1"). Tested in `core-api/tests/dyn_view.rs` (9 tests).
    CAUGHT by the user asking directly: the first pass tested `find` (returns the right pointer) and a
    generic field read (given a `&dyn Topic` directly) SEPARATELY, never the actual end-to-end path
    (`find` by `Tid`, then read a field through what it returned) together in one test, so a mix-up
    between two different topics' data behind `find` would not have been caught. Added
    `a_consumer_that_looks_up_a_topic_by_tid_reads_that_topics_own_data` (two different message types,
    `Reading` and `Status`, each with a published value; looks each up by its own `Tid` and checks the
    field value, not just the pointer). Workspace: 433 tests (was 423). clippy and fmt clean.

## WHERE WE LEFT OFF (2026-11-24)
Everything builds, 433 tests pass, clippy and `cargo fmt --check` are clean. Items 83 to 86 (queued topics,
events-on-the-bus step 1) are committed by the user (`7e8c5d1`, `c8a96d2`). Uncommitted (user commits, not
me): `.claude/progress.md`, `core-api/src/bus.rs`, `core-api/src/lib.rs`, `core-derive/src/lib.rs`,
`docs/bus.md`, `docs/design.md` (all modified), `core-api/src/message.rs`, `core-api/tests/message_derive.rs`,
`core-api/tests/dyn_view.rs`, `core-derive/src/code.rs`, `core-derive/src/message.rs` (new) — items 87 to 90:
the `bus::Message` derive, the APID-to-TID rename, the `core-derive` file split, and TIDs/the dyn view step 1.
See `docs/bus.md` for the full bus design and its roadmap ("Then, in this order: ...").
The user is learning Rust (comes from C++): explain syntax and generics as they come up, and follow
CLAUDE.md (one thing at a time, simplicity, callbacks only when needed, "do we really need this?", and
after any edit name each file touched and what was done in it). `latest-chat.md` is updated only when the
user says the session is ending.

Where the code is: `Core<P: Platform>` (`core/src/lib.rs`) has `start`, `tick` (the log writer's step, the dropped
report, then the watchdog feed), `stop`, `reset`, `files`, `watchdog`; `vee_core::slot::Slot<M>` is the per module
bookkeeping (`init`, `report_suppressed` — also resets the module's per-tick throttle counting, see item 85 — and
`step`). The OSAL has the interfaces (`clock` with `sleep_until_us`, `files`, `syslog`, `panic_report`,
`watchdog`, `platform`), the fakes, and `linux/` (`std_clock`, `std_files`, `std_syslog`, `std_panic` the panic
hook, `std_watchdog`, `std_alloc_guard`, `LinuxPlatform`). `modules/example` is the sample module, `modules/watcher`
reads its state topic. `sample-deployment` (S2a to S2e done) is the first executable: `start` (the whole start
sequence), `Exec::run` (the loop: absolute boundaries, an overrun skips, logged), `VEE_SAMPLE_TICKS=N` to end
through `Core::stop`.
The software bus (`core-api/src/bus.rs`) is DESIGNED in full (`docs/bus.md`, item 58) and partly written: state
topics (`StateTopic`/`StateWriter`/`StateReader`, item 58's first slice, DONE 2026-11-23) and queued topics
(`QueuedTopic`/`QueueWriter`/`QueueReader`, items 83 to 85, DONE 2026-11-24), both with `Module::In`/`Out` wiring
and a hand written loop test each (`core/tests/bus_loop.rs`, `core/tests/queued_bus_loop.rs`). `Ev`'s bus queue
and filter (`core-api/src/ev.rs`, `Ev::bus`, `set_filter`, `flush_bus`, item 86, DONE 2026-11-24) are written and
unit tested but NOT wired into `Core::tick`: there is no events topic yet to flush into (nothing sizes its `N`
readers until telemetry exists). The `bus::Message` derive (`core-api/src/message.rs`, `core-derive/src/message.rs`,
item 87, DONE 2026-11-24, split into its own file item 89) writes a message type's `WIRE_LEN`/`LAYOUT`/`encode`
(inherent) and `impl message::Message` (the trait generic code needs, item 90). `bus::Tid` (renamed from APID,
item 88) and `bus::Topic` (item 90, DONE 2026-11-24), the dyn view: `layout()`, `latest_bytes()`,
`read_latest(index)` (bundled so a caller cannot pair the wrong field with the wrong bytes), `pop_encoded(reader,
out)`, `find(topics, tid)`; implemented for `StateTopic<T>` and `QueuedTopic<T,N,D>`. NOT yet written: the core
commands for the events filter (also what will wire `flush_bus` into `Core::tick`), the recorder, telemetry,
FDIR, `exec!` (which is what will build the real `(tid, &dyn Topic)` array and the table that reads it).
The blocking event log write is gone (item 74); the two sample overruns on WSL were late wake-ups (item 75); none
on native Linux.

## Next
Open choices, in the order I would do them, all for the user to pick:
1. The file service (thread, bounded request queues): removes the blocking `append` from `Core::tick` and the
   overruns above. The biggest piece: a new `Platform` associated type. After it, assert `Core::tick`
   allocates nothing.
2. The bus (item 58), state topics first, as the sample module's first real use.
3. Time and Health (overrun detection as an event, the counters), commands (handled in the module's step,
   item 32), the exec's stop path on SIGTERM (decided NOT to handle: item 54), the ground tool.
Older plan, still relevant: STRETCH GOAL, only after everything else works (noted in the design doc,
Executive Requirements): a threaded module type for slow work (image processing, say) that gets its
own thread or core, talking through bounded queues like the file service.
Possible follow-ups: `trybuild` for the derive's compile-fail cases (adds a dev dependency);
the command derives (`Command`, `CmdSet`) could use the same idea (opcode = enum value).

## Decisions to remember
- Debug and test only features (the allocation guard, fakes, the test hooks) must be invisible to the code they
  check: no production code is added or bent to suit them (user, 2026-11-20). Put the accommodation in the debug
  code itself.
- `Clock` stays ONE trait, with `sleep_until_us` on it (user, after the session of item 59): modules already cannot
  reach it (they depend only on `vee-core-api`, which does not depend on the OSAL), and the core is not meant to be
  edited by developers, so there is no need to forbid waiting there by the type system.
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
  setting (item 20: `reset_log::RESET_LOG_MAX_RECORDS`), passed to `start` as a
  parameter.
- The reader's rules: any problem (corrupt record, misordered records, count overflow) means the
  history cannot be trusted, so the result is `ResetInfo::first_boot()` plus the problem. A first
  record that is a `Shutdown` is accepted (its `Boot` was before the part read). First boot and
  "no trustworthy history" both give `previous: Unknown`, `consecutive_unclean: 0`.
- Non-fatal problems found at startup (partial record, bad CRC) are returned in `start`'s result,
  not dropped. The caller turns them into events once the event service exists.

- `Core<F: Files>` stays one parameter for now (agreed with the user, 2026-11-19). When a SECOND OSAL
  service arrives (the watchdog for Health, or the file service's thread), bundle them in one trait
  so `Core` keeps a single parameter: `trait Platform { type Files: Files; type Watchdog:
  Watchdog; }` and `Core<P: Platform>`; the real platform and the test platform implement it once.
  Do not add it before then. Other things `Core` will need: Health holds `ResetInfo` (a value, from
  the start sequence); the file service needs `F` usable across threads (`Send + Sync`, a bound);
  the Bus, Command Router and Tables depend on the deployment's types, so `exec!` must supply them
  (as type parameters or by generating those pieces): decide when `exec!` is designed. `Core` needs
  no clock type, because `tick(now)` takes the time.

## Open questions
- Shutdown reasons beyond `Commanded` and `Signal`.
- Whether a lone `Shutdown` as the first record of a log shorter than the tail window should be
  an error. Today it is accepted, because the reader cannot tell a cut window from a short file.
- Event logger: one file or one per source?
- Time correlation event (monotonic to TAI): when it is emitted, what it carries.
- Whether to add call-site location to events (see item 11).
- How a module waits for a command result without blocking (see item 19). Part of the command design.

## Not started
command routing, the software bus (designed, item 58), tables, the file service, `exec!` (the sample
deployment is its hand written precursor), more modules, the `bus::Message` derive, the ground tool. (The
`module-types` crate was dropped: no mission wide types crate, item 58.)
