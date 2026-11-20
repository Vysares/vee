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

## WHERE WE LEFT OFF (2026-11-19)
Everything builds, all tests pass, clippy (with the strict lints of item 29) is clean. Committed by
the user: `a3c2ef6` (stricter lints, basic clock: items 28 and 29) and `92a65d3` (the suppressed
reporter: item 30). NOT yet committed: items 31 to 34 (the queue depth change, the "Core services"
section and the commands-in-step change in `docs/design.md`, the loop test `core/tests/exec_loop.rs`,
the `cargo fmt` whitespace changes, the system log step 1 and its "System Log" design section, and
this file). The user commits.
The user is learning Rust (comes from C++): explain syntax and generics as they come up, and follow
CLAUDE.md (one thing at a time, simplicity, callbacks only when needed, "do we really need this?",
and after any edit name each file touched and what was done in it).

## Next
UPDATE 2026-11-19 (latest): the user approved the system log in 4 steps (item 34). Step 1 is done;
next is step 2 (the queued mode), unless the user redirects. After that, the plan below.
UPDATE 2026-11-19: the loop test is DONE (item 33). What is left of that plan: see what `Core` needs to
own (the loop has a placeholder for `core.tick(now)`), then design `exec!` from the loop (the generic
`Slot<M>` and the ordered calls). The smallest domains the loop needs first: Time, the Events drain
(`Ev` to the event log, `take_dropped` as an event), Health (overrun, watchdog). Commands are next
after that, with the new rule of item 32 (handled in the module's step).
UPDATE 2026-11-18 (still true): the clock is done (item 28). The scheduler does NOT live in core
(see item 28). Order was: (1) DONE (item 30) the suppressed reporter; (2) a `Core` struct with
`tick(now)`; (3) the hand-written loop; (4) design `exec!`. Now (3) comes before (2).
The paragraph below is the older plan; where it conflicts with item 28, item 28 wins.

**The plain hand-written scheduler in core** (step 2 of the Ctx and exec plan; then `exec!`, which
generates what the scheduler does): base rate, module rates that divide it, a fixed order, a tick
counter; each tick it calls `new_tick` on every source's throttle and sends `EventsSuppressed` events,
reads the clock once per module call, builds each `Ctx`, calls `init` once for each module and
`step` at each module's rate. Test it with two or three test modules and a fake clock (the OSAL has no
clock trait yet: write one, `Clock { monotonic_us() }`, with a fake). Design points it will raise:
how the scheduler stores modules of different types (a trait object `dyn`, a generic list, or
generated by `exec!`; `exec!` generating the per-module glue is probably the answer), the order
of `init`, the `started` flag, overrun detection, where `EventsSuppressed`'s text is built.
Later: `command()`, the file service thread (the user said to hold off), the main-thread half of
the event logger, the startup sequence. STRETCH GOAL, only after everything else works (noted in the
design doc, Executive Requirements): a threaded module type for slow work (image processing, say)
that gets its own thread or core, talking through bounded queues like the file service.
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
command routing, software bus, tables, events, file service, `exec!`, modules,
`module-types`, `ground-tool`.
