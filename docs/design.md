# Vee Requirements
Vee (the Very Economical Engine) is a super lightweight flight software framework written in rust. It targets Linux-class systems (anything where rust's std is available, including many RTOSes).

This repo contains the core, a set of standard modules, and an example mission configuration.

*code samples are pseudocode / examples.*

## General Requirements
- No std or unsafe is enforced everywhere except the OSAL. Host-side tools (ground-tool, proc-macro crates) may use std.
- Static allocation is the goal: every queue, table, and buffer is sized at compile time (const generics, sizes from `exec!`).
- Allocation at startup is tolerated only where static is not practical.
- No allocation after startup. This is a rule for the code, and it is checked in development, not enforced in flight: a global allocator (`osal::std_alloc_guard::GuardAlloc`, declared by the deployment's `main` with `#[global_allocator]` in the builds that want it: dev, SIL and CI) aborts with a fixed message on any allocation once the start sequence calls `std_alloc_guard::arm()` (the `started` flag). A flight build does not declare it, so there is no guard and no Cargo feature: the declaration is the switch. The guard also counts allocations per thread (`allocations_on_this_thread`), which tests use to assert that a piece of code allocates nothing. An allocation after startup that succeeds is a bug to find before flight, and killing the process for it would turn that bug into an outage.
- Keep dependencies to a minimum.
- Developers should never touch command, table, or message raw bytes. The only time they should deal with raw bytes is I/O.

## Planned Crates and Dependencies

| Crate        | Depends on                                              | Contains                                                          |
| ------------ | ------------------------------------------------------- | ----------------------------------------------------------------- |
| exec         | core, core-types, modules, mission's module-types, osal | The constructed deployment and application.                       |
| modules      | module-types, other modules' module-types, core-api     | Module code (as libs). I/O through its step's `ctx`. Includes the standard modules (such as the ingest/link module). |
| module-types | core-api                                                | Each module's commands, messages, events, tables.                 |
| core         | core-api, core-types, osal                              | Message bus, command router, event service, table service, file service |
| core-api     | core-derive                                             | shared types and traits between core and modules                  |
| core-types   | core-api                                                | The core's own commands, events, messages, tables.                |
| ground-tool  | core-types, mission's module-types                      | Command and telemetry tool for testing. May use std.              |
| osal         | -                                                       | Thin OS abstraction layer. Only flight crate that allows std and unsafe. |
| core-derive  | -                                                       | proc-macro derives                                                |

## Core
The core provides a minimal set of utilities to each module.

### Core services
The core is split into domains. Each is a plain struct with one job and its own file in the `core` crate, sections below. They are the seven services:

| Domain | What it provides | Hooks (planned) | State today |
| --- | --- | --- | --- |
| Health | The reset log (how the last run ended, `ctx.reset`), overrun detection, the watchdog kick, and the core's own counters | `start`, `tick`, `stop` | the reset log (`reset_log`) and the watchdog feed (a feed at the end of every `tick`; `WatchdogUnavailable` once per failure streak) exist; overrun detection and the counters do not |
| Time | The clock, read once per call and fixed into each `Ctx`; the tick count. Later, the monotonic to TAI correlation event | `tick` | `Clock` in the OSAL |
| Events | The event queue (`Ev`), per event throttling, the report of suppressed events, the event log | `start`, `tick` (drain), `stop` (last drain) | `ev`, `throttle`, `suppressed`, `events` (`report_dropped`), `event_log` (file side), and `Core::tick` taking events out and writing them to the event log (blocking, temporarily) |
| Message Bus | One writer per topic: state topics (latest value, `seq`, readable by any module that lists them) and queued links (a queue per reader) | none planned: a send queues, a module drains in `step` | not started |
| Command Router | Commands to a module's `command()`, acks, and a result for the sender's `seq` | `tick` (route into the modules' command queues) | not started |
| Tables | The table sets, the one load buffer and one dump buffer | `start` (load), `tick` (follow file results), `stop` (dump) | not started |
| Files | The file service: a thread, bounded request queues, results back. Clients are Tables, the event log and modules (`ctx.file`) | `start`, `tick` (collect results), `stop` | `Files` in the OSAL (blocking, for startup and shutdown); `file_service::FileService` (the worker thread, `submit` and `poll`) and the types in `core_api::file` exist, and nothing uses them yet (the event log writer is next) |

- The reset log is in Health because it is related to it, but it is not a service: nothing calls it during a run. `start` produces `ctx.reset` and `stop` records a clean shutdown. A panic leaves the last run `Unknown` (see Faults).
- The Bus and the Command Router stay separate. The bus is publish and subscribe: state topics (the latest value) and queued links (a queue per reader). The router is point to point (target and opcode) with acks and results. They share only `queue::Queue`.
- Files is the only domain that owns threads. (Design, decided 2026-11-20: a request and a response are fixed size values moved by value through two bounded queues, with the data inline up to `FILE_DATA_MAX_BYTES`; each CLIENT has its own file service, with its own worker thread, and its queues' depth is the client's limit on outstanding requests, so they cannot overflow by construction. The file service's errors are its own (`core_api::file::Error`: besides the OS's kinds, `Busy` and `TimedOut`), mapped from the OSAL's `FileError` in the core.) Its clients never block: a request goes into a bounded queue (a full queue is an immediate nak or a counted loss) and the answer comes back on a later tick.
- Health reports what it finds as events, so it sends through Events. Events and Tables both send through Files.

The executive schedules the core like a module, every tick and with no divisor (`core { budget: .. }` in `order`), but the core is not a `Module`: it owns `Ev` and the logs, and its own events skip the throttle. The core is one struct, `Core`, that owns the domains and calls their hooks in an order the core fixes, so a deployment cannot get it wrong. It has one entry point per tick, `core.tick(now)`, and no begin and end split: the loop comes back around, so draining events, the watchdog kick and overrun detection (compare the start times of consecutive ticks) can happen at the top of the next tick. Start and shutdown are separate calls, `start` and `stop`. `exec!` generates the loop, because only the deployment knows every module type (`Module` has an associated type, so no list of modules fits in `core`). Each tick:

1. For each module, report its suppressed events (`suppressed::report`, generic over the module's event enum). This also zeroes that module's throttle, which starts the tick's count.
2. `core.tick(now)`.
3. For each module due this tick, read the clock, build its `Ctx` and call `step`. The module's queued commands are handled in this step (see Command Routing), not every tick.

`Core` (`vee_core::Core<F: Files>`, in `core/src/lib.rs`) exists with the Events domain and Health's reset info. `Core::start` and `Core::stop` are its startup and shutdown (see Executive Requirements). It owns the event queue (`pub ev`, which the executive lends to every `Ctx`), the file system `F` and the event log path. Its `tick(now)` does: (1) take every event out of the queue and append it to the event log, then (2) `events::report_dropped`, in that order, so the report has room in the queue. Until the file service exists the append is a blocking write in the tick, which the design forbids in the loop: it is temporary, and the file service will replace the `append` call with a request, leaving the loop as it is. A failed append loses that event (nothing else can be done with it) but the queue is still emptied, and the failures are reported once per tick to the system log. The events of a tick's modules are written in the next tick, and the suppressed reports in the same one. Not written yet: `exec!`, the other domains inside `core.tick` (their order is decided as each is written), and the Bus, Router, Tables and Files domains.

### Software Bus
The bus has its own document: [bus.md](bus.md) (split out 2026-11-23; nothing is written yet). In short: typed topics with explicit APIDs, one writer per topic, state topics (latest value) and queued links, access listed per module and wired at compile time, a module declares the types it produces, and events will be a topic fed by the event service.

### Command Routing
- The core provides for command routing to modules.
- Each module, and the core, define their own commands. Modules define these in their own crate (no separate types crate: the core reaches them through the generic `Slot<M>`, as it does a module's events, and the ground tool builds its dictionary from the same crates).
- Every command has a `target` (the module it is for) and an opcode. Opcodes are unique within a module.
- The bus carries commands in a neutral internal form, `RawCmd { target, opcode, args }`. The core knows nothing about CCSDS.
- Modules that bring commands in from outside (the ingest/link module, an RTS or ATS module) hand them to the router through a Command Sink.
    - A CCSDS ingest module owns the mapping from APID to `target`, in its config.
    - Each command sink has a unique ID defined in the deployment, to distinguish between command sources such as ground and RTS.
- The command router decodes and validates each command once, then passes the typed command to the target module.
- The core generates an ack or nak as an event on:
    - Valid/Invalid command received by the router
    - Success/Fail/Success(pending), after the module executes the command.
- Acks and naks are ordinary text events (see Events), with the command's details (sink, target, opcode, seq, reason) written into the text. The ground reads them like any other event.
- Inside Vee, a module that needs to know whether a command succeeded (an RTS that waits for success) does not read events. The router gives the sender a result for the command's `seq`, the same for every sink, and the sender collects it without blocking: it submits, keeps the `seq`, and checks for the result on a later cycle (everything runs on one thread). Not designed yet.
- Each module has a fixed-depth command queue. A full queue is an immediate NAK.
- The router puts a command into the target module's queue when it arrives. The module handles it in its own step (see Module Requirements), so how long a command waits depends on the module's rate. The ack for "command received" is sent when the router queues it, and the ack for the outcome is sent after the module has handled it, which can be a period or more later. Not designed yet: how the exec calls `command()` inside the step (for each queued command before `step()` in the same call, with one `Ctx`, or another way), and whether one step may handle every queued command or has a limit.

``` rust
/// in ats-types.rs, for example

#[derive(Command, Clone)] #[opcode(0x01)]
pub struct Activate { pub which: Which, pub slot: u8 }

#[derive(Command, Clone)] #[opcode(0x02)]
pub struct SetLimit { pub max_rts: u16, pub timeout_ms: u32 }

#[derive(CmdSet)]                     // one enum per module: the union of its commands
pub enum AtsCmd { Activate(Activate), SetLimit(SetLimit) }
```

- `#[derive(Command)]` generates the fixed-layout `encode` and `decode` between the struct and `args`, the opcode constant, and a hook for `validate`.
- `#[derive(CmdSet)]` generates `AtsCmd::decode(&RawCmd)`, which matches on the opcode and calls the right variant's `decode`. Its result is the typed enum, or an error for an unknown opcode or bad length.

The router (core) decodes and validates once, and the module only sees the typed result:

``` rust
// Called by the router, in core. Generated glue, one per module.
fn accept(raw: &RawCmd) -> Result<AtsCmd, CmdErr> {
    let cmd = AtsCmd::decode(raw)?;   // generated by CmdSet
    cmd.validate()?;                  // calls each variant's validate hook
    Ok(cmd)
}

// Written by hand in the module. This is the module's dispatch table.
fn command(&mut self, c: AtsCmd, ctx: &mut Ctx, r: CmdRef) -> CmdResult {
    match c {
        AtsCmd::Activate(a) => /* typed fields */,
        AtsCmd::SetLimit(l) => /* typed fields */,
    }
}
```

### File Service
Decided 2026-11-20. Written so far: the types, the service (`submit`, `poll`, `wait`, the worker), `FileDevice`, the OSAL's `channel` and `thread`, and the event log writer (the first client). Not written: the other clients and `ctx.file`.
- It does the blocking file I/O of the run on one worker thread (`vee-file`), so nothing on the main thread waits on a disk. A client submits a request, carries on, and collects the response on a later tick. It is the one core service that owns a thread.
- The types are in `core_api::file`, because modules need them: `Op` (`Append`, `WriteFile`, `ReadFile`; `Delete` when the OSAL's `Files` can do it), `Response = Result<Data, Error>` (no tags: one request is in flight, so a response is the answer to it), `Path` (strict: never cut, `file::path` refuses a path that is empty, not ASCII or too long) and `Data`.
- A request and a response are fixed size values that are MOVED through the queues, with the data inline, up to `FILE_DATA_MAX_BYTES` (default 4096, so a table fits). Nothing is allocated, nothing is lent to the worker thread, and a request that times out leaves no buffer stuck on the other side (this replaces the earlier idea of a load buffer and a dump buffer that belong to the service while a request is in flight). A larger file is sent in pieces.
- The errors are the service's own (`core_api::file::Error`). Besides the ways a file operation fails they include what only the service can say: `Busy` (the queue is full) and `TimedOut`. The OSAL's `FileError` stays the OSAL's, and the core maps it (`file_service::error_from`), so modules see only core-api.
- The code is `core::file_service`: `FileService::start(files)`, `submit(op) -> Result<(), Op>` (a full queue gives the op back), `poll() -> Option<Response>`, both without waiting, `wait(timeout_us)` (shutdown only), and the pure `handle(files, op) -> Response` that the worker runs. `Platform::Files` is `Clone + Send + 'static`, and the core gives each worker a clone. The OSAL provides only what any thread needs, `channel` (a bounded queue) and `thread` (start one). The worker has its own handle to `Files`.
- A generic worker (any blocking I/O, such as a UART or a socket, on its own thread with a handler closure) is a good idea, but it is NOT built until a second blocking use exists (decided 2026-11-20: the event log no longer needs its own thread, so the file service is the only one). When it is, `FileService` is almost already it: only the call that runs a request is file specific. Shutdown needs a wait with a timeout either way: `Core::stop` flushes the event log writer (it sends what is queued and waits up to `EVENT_LOG_TIMEOUT_US` for each response), which uses the OSAL channel's `Receiver::recv_timeout(timeout_us)`: only the platform can wait with a timeout, and the core has no clock to sleep with. It is the one blocking wait in the core, and only at shutdown.
- One file service, with its own worker thread, for each client (the event log writer, tables, and each module that uses files; decided 2026-11-20). A client's requests are done and answered in order, so a request sees what its earlier requests did. This keeps a slow request of one client from delaying another's (a table write cannot delay the event log), and it removes the routing of responses to clients and any rule that sums clients' limits. The costs are a thread for each client (each is blocked almost all the time) and that there is no order between clients: a file should have ONE client that writes it (the event log file belongs to the event log writer, a table file to tables, a module's files to the module), and another client may read it while it is written, because readers already tolerate a partial last record. The disk is still shared, so a stalled disk slows every worker, and an append with a sync takes about 2.5 ms on the development machine. A deployment that has too many threads can give two quiet clients one service, since the code does not care.
- Queue depth: one slot in each queue, because one request is in flight (see above). Each slot is about `FILE_DATA_MAX_BYTES` of memory. (An earlier design had a depth `N` and a response queue of `N + 1`, after a test found that a stale request and a fresh one could finish together and lose the second response; with a request that keeps its slot until the worker finishes, that cannot happen.)
- The clients, each with its own service:
    - The event log writer (`core::log_writer::LogWriter`, written): each tick it collects the response, and if its device is free builds one batch (`event_log::batch`: `peek`, encode, `add_bytes` until a record does not fit, `pop` only after it was accepted) and sends one `Append`. While a write is in flight the events wait in the log queue, which counts and reports what it drops. A failed batch is lost and reported once per streak to the system log.
    - Tables (core), limit 2 (a load and a dump): `ReadFile` into a slot, and `WriteFile` from one.
    - Modules, through `ctx.file`: `request(op) -> Result<Tag, Busy>` and `poll() -> Option<Response>`, for modules whose config lists `uses: [file]`, each with its own limit and its own small response queue that the core fills, routing on `tag.source`.
- Two standard modules are the operator's way to the files, and they are separate (decided 2026-11-20). Both are modules, not core code: they touch no core state, their path policy is the mission's, and a deployment that does not want them does not list them. Each has its own file service.
    - FILE is the operator's handle on the filesystem itself: list and make directories, delete, rename or move, copy, and get a file's size and checksum. It never moves whole files. A listing or a checksum goes back to the operator as a typed topic that the link module downlinks, so the core needs no knowledge of the downlink. The deployment gives it an allow-list and a deny-list of paths; the core's own files (the reset log and the event log, which have a single writer each) are on the deny-list. File operations are ground commanded. When its service is full (`Busy`) FILE replies with an immediate NAK of that command and does not run it or park it, so the commands that do run, run in the order they arrived, and ground paces file commands by their completion acks. Commands that are not file operations are handled in the same step and are never held up by the filesystem.
    - CFDP moves whole files to and from ground, in chunks, over the link.
    - Table upload is the two together: CFDP puts the file on disk, FILE can inspect or tidy it, and the core's table command loads it into a slot.
    - The service needs more operations for them (not written): for FILE, delete, rename, copy, list a directory, make a directory and a checksum (the OSAL's `Files` has none of these yet); for CFDP, a read at an offset (`Files::read_at` exists) and an append or a write at an offset, each up to `FILE_DATA_MAX_BYTES`.
- Reading a file in pieces is driven by the CLIENT, not the worker (decided 2026-11-20): the worker stays stateless, as the OSAL's `Files` is. The operation is `ReadAt { path, offset, len }` (not written), with `len` up to `FILE_DATA_MAX_BYTES`, and a chunk shorter than `len`, or empty, is the end of the file (a file whose size is an exact multiple of the chunk costs one extra empty read). The client asks for the next chunk when it can use it, so flow control and giving up come free, and a client with more than one slot can ask for several chunks ahead. The price is that a chunked read of a file that is replaced in the middle could mix old and new bytes, so a file being transferred is not written: the single-writer rule, as CFDP expects of its source.
- EVERY file service is strictly ONE request in flight (decided 2026-11-20, replacing the earlier `N`): its request queue and its response queue have one slot each, and a client has one outstanding request at a time, so the one outstanding request is the one in flight and a response is always the answer to it. There is no limit parameter, no table of outstanding requests, and no tag to match a response to a request (the client already knows what it is waiting for). A request that times out is reported to the client as `TimedOut`, but the service stays BUSY until the worker really finishes it (a stuck disk keeps it busy, and new requests are refused as `Busy`, which is the true answer), so a late response is always the stale one and is dropped. A client is a `FileDevice` (the service and that small state: idle, waiting with a deadline, or timed out and still waiting), with `request(now, timeout, op) -> Result<(), Busy>` and `poll(now) -> Option<Result<Data, Error>>`. `Busy` is the ordinary answer when it is full, and each client deals with it in its own way: the event log writer leaves its events in the log queue, and a command module NAKs the command. More at once comes from MORE DEVICES, not a deeper one: a client that needs two streams owns two devices. CFDP starts on ONE device: a serial transaction does one chunk at a time, in either direction, so reads and writes share it; a second device is for concurrent transactions or for read-ahead, and nothing asks for either yet (a faster link would first want a larger `FILE_DATA_MAX_BYTES`).
- Timeouts are the client's, kept by its `FileDevice`: a request with no response by its deadline is answered `TimedOut`, and the response that comes later is dropped because the device knows it is the stale one.
- Startup and shutdown still use the OSAL's blocking `Files` directly: the reset log and the event log are opened before the loop, and `Core::stop` writes the shutdown record, then waits (with a limit) for the service to finish what is queued. That wait is the one blocking call, and only at shutdown.
- A panic aborts the process (see Faults), and what the service had queued is lost. That is accepted, as it is for the event log.

### Tables
- Each module defines its own table schemas (structs) in its types crate.
- Table schemas use plain base types (u8, u16, string, etc), so operators can use them easily.
- Modules have Table Sets. A set holds N slots of one schema. The core owns the sets.
- The core provides the commands, using the file service:
    - load a table file into a slot of a set
    - dump a slot of a set to a file
- Loading into a slot has no effect on the module. The module implements its own Activate command, which copies a slot into its active table. Activate runs on the main thread, so it is a plain copy between tables already in RAM.
- New table loads only come from ground. Switching between tables already loaded in a set is how tables change in normal operation.
- The core owns one load buffer and one dump buffer, each sized to the largest table schema in the deployment (computed by `exec!`).
    - Load: the file service reads the file into the load buffer and returns it through its response queue. On the core's `step()`, the core decodes and validates the buffer and copies the result into the slot.
    - Dump: on the core's `step()`, the core serializes the slot into the dump buffer and sends it to the file service to write.
    - The buffer belongs to the file service while a request is in flight. A load or dump command that arrives while its buffer is in flight is an immediate NAK (busy).
    - A file service timeout must still return the buffer, with an error, so a failed request cannot block later ones.
- Slots are only touched by the main thread, so table sets need no lock. The only cross-thread handoff is the buffer, which moves through the file service's existing queues.
- Tables listed in the deployment's `boot` list are loaded during `init`, one at a time, through the load buffer.

``` rust
/// in core-api

// A set of tables sharing one type: T. The core owns these. Main thread only, so no lock.
struct TblSet<T, const N: usize> { slots: [Option<T>; N] }

// A single active table and its source index in the table set. For use in a module's lib.rs.
pub struct Active<T> { tbl: T, source: Option<u8> }

/// in <module>-types.rs
struct SurvivalTblEntry
	thermistor: u8,
	low: u16,
	high: u16,

struct SurvivalTbl // the table set's schema
	entry1: SurvivalTblEntry,
	entry2: SurvivalTblEntry

survival: TblSet<SurvivalTbl, 2> // table set with 2 slots

/// in <module>'s lib.rs
active: Active<SurvivalTbl>
```

### Reset Log
- The core keeps a reset log: the full history of resets and why each run ended. It does not depend on systemd or any other supervisor.
- The log is an append-only binary file of fixed-size records. Records are never rewritten. It uses the same fixed-layout encode and decode as commands and tables.
- Only boots are counted. A record is one of:
    - `Boot { count }`, appended at startup.
    - `Shutdown { reason }`, appended on a clean shutdown (commanded, signal, etc). It has no count: it belongs to the most recent `Boot` before it.
- Each record is a tag byte (which variant), that variant's fields, zero padding up to the size of the largest variant, then a CRC. So all records are the same size, and the last K records are at a known offset from the end of the file. Encode and decode write and read each field explicitly (no transmuting the enum). `ResetLogEntry`'s codec is hand-written. A derive for tagged enums can wait until a second type needs one.
- The CRC is CRC-16/CCITT-FALSE (polynomial 0x1021, initial value 0xFFFF, no reflection), implemented once in `core-api`. It is the same CRC CCSDS uses, so the ingest module and table files can reuse it.
- If the last record is a `Boot`, that run ended uncleanly (crash, kill, power loss). Its reason is `Unknown`, inferred rather than stored.

``` rust
// core-types
enum ResetLogEntry {
    Boot { count: u32 },
    Shutdown { reason: ShutdownReason },
}   // each record is encoded to the same fixed size, plus a crc
```

- On startup, before any module's `init`:
    1. The core reads the tail of the log through the OSAL: the last `Boot` (for its count), and whether a `Shutdown` follows it.
    2. The core appends `Boot { count + 1 }`.
    3. The core emits an event reporting the previous shutdown reason and the new reset count.
    4. The core makes the result available to modules as `ctx.reset` (see below). This is `Core::start`, and the core keeps the result (`core.reset()`).
    5. If the log cannot be read or written at all, the core carries on with `ResetInfo::first_boot()` and sends `CoreEvent::ResetLogUnavailable` with the file error (see Executive Requirements).
- These reads and writes are blocking calls made directly from the core through the OSAL. They only happen at startup and shutdown, so they do not go through the file service and do not affect the tick loop.
- A missing log (first boot) starts the count at 0. A file whose size is not a multiple of the record size (a crash mid-append) has its partial last record ignored and reported as an event. A record with a bad CRC, or any other unreadable content, is reported as an error event and the count restarts. It is never silently treated as valid.
- The log has a size cap. It is checked at startup: if the cap is passed, the core renames the log to `.1` and starts a new file with the new `Boot { count + 1 }`. The count carries forward by itself, because it lives in the `Boot` record.

#### Reset info for modules
- During `init`, modules read `ctx.reset`, which holds:
    - `count`: the reset count for this run
    - `previous`: how the previous run ended: a clean shutdown with its reason (`Commanded`, `Signal`, ...), or `Unknown`
    - `consecutive_unclean`: how many resets in a row, up to now, ended `Unknown`, counted back from the tail as `Boot` records with no `Shutdown` after them (bounded by the tail read at startup)
- The log only records how the run ended. It does not hold module state. A module that resumes after a clean reset keeps its own state across resets. For example, a mission's mode manager module (Vee itself has no modes) persists its current mode each time the mode changes, as a file written through the file service.
- With that, such a module decides what to do: after a clean reset it resumes from its persisted mode, and after an `Unknown` end it reacts (for example, going to safe mode, and staying there once `consecutive_unclean` passes a threshold from its table).

## Events
- The core exposes an API for modules to emit events, through `ctx.ev`.
- An event is a code and a short text. Inside a module, a code and a sentence is enough. If more detail is needed, add another code: codes are cheap. The text can carry values (`"open failed: {path} ({err})"`, written with `format_args!`), so the ground can read them. Events carry no typed args.
- Every event has the same envelope: source (the module), code, severity, a monotonic time, and text of a fixed size (`EV_TEXT_MAX_BYTES`, 64 by default, set at build time: see Build-time settings). Every event is the same size on the wire and on disk: shorter text is zero padded.
- The text is a `FixedStr<EV_TEXT_MAX_BYTES>` (`core-api`): ASCII only, exactly that many bytes on the wire, zero padded, ending at the first zero byte. Text that is too long is cut off and the event is still sent, because the ground log must see it (see Faults). A cut-off text ends in `~` (`fixed_str::TRUNCATED_MARK`, replacing the last kept byte), so a reader can see it was cut. A text of exactly the maximum length is not marked. Writing text with `format_args!` goes through `fmt::Write` on `FixedStr`, which cuts off the same way and never fails: a character that cannot be in the text (non-ASCII) becomes `?`. No allocation happens in flight.
- The code is the value of an enum variant, on an enum marked `#[repr(u16)]`, one enum per source (a module's, or the core's). A code is a `u16`. Values are implicit (0, 1, 2, ... in the order written) and Rust refuses two variants with the same value, so codes are unique within an enum. The catch: a code is then the variant's position, so reordering variants or inserting one in the middle renumbers the codes after it, and old logs would decode wrongly. New events go at the end. A variant may be given an explicit value (`Started = 0x01`) to pin its code.
- Each code has a fixed severity: info, warn, err. It is given once, on the variant, and the caller of `ctx.ev.send` never picks it.
- `#[derive(event::Code)]` (core-derive; the trait is `event::Code`) generates the lookup (below). The core's own events use it exactly as a module's do.
- Command acks and naks are plain events (see Commands). They have no special codes, kind bits or fields. They are written the same way as any other event, with the details in the text.
- Throttling: a source can send each of its events at most `EV_MAX_PER_TICK` times per tick (a tick is one executive cycle, one minor frame; events sent from a module's `command()` count, since commands run in the module's step, which is inside a tick). The default is 5, at most 255, and it is a build-time setting. Sends past the limit are dropped. At the start of the next tick the core sends one `CoreEvent::EventsSuppressed` (Err) for each event that was suppressed, with the source, the event's code and how many sends were dropped (stopping at 255 minus the limit), however many there were. The counters are `core_api::throttle::Throttle`: one `u8` counter per event, found by the event's position (`index`), so there is no table to fill up and one event can never throttle another. Every source's throttle is the same size, `EV_MAX_CODES` counters (default 256, a build-time setting), and a source uses the first few. `#[derive(event::Code)]` fails the build if an enum has more events than that. The throttle belongs to the source, so it lives with the source's handle behind `ctx.ev`, which checks it before calling `Ev::send` and can only send its own module's event enum. The core's own events do not go through a throttle, so `EventsSuppressed` cannot be suppressed. Not written yet: the handle (it needs `ctx` and `exec!`).
- The event service is `core_api::ev::Ev` (in core-api, not the core, so `Ctx` can hold a reference to it): one `core_api::queue::Queue<Event, N>` as a public field, `log` (depth the build-time setting `EV_LOG_QUEUE_DEPTH`, default 1024, 80 KiB at 80 bytes a slot), with no I/O of its own (only the system log write below) and no clock. There is no cap per source, only the per event throttle. `send(source, monotonic_us, code, text)` builds the `Event`, writes it to the system log (the write comes first, so an event the queue cannot take is still seen), and pushes it to `log`. The log writer `peek`s at the next event and `pop`s it once its batch has room. The `Queue` counts the pushes it refused (`take_refused`): a full queue drops the new event (the oldest are kept, because the first events of a problem are the most useful) and the core sends the count on as an `EventsDropped` event (`3 log events dropped: the queue was full`), so a loss is never silent. `send` never fails: text that is too long is cut off and marked, and a value in the text that fails to print leaves the text up to that point. Modules never get an `Ev`: they only get the `ctx.ev` handle, which can only send.
- Telemetry and other subscribers are bus work (decided 2026-11-20, changed 2026-11-23; see [bus.md](bus.md), "Events on the bus"): there is NO telemetry queue in the log path. Events will be a bus topic of type `Event`, written by the event service. `ev.send` ALSO pushes the event to a second queue for the bus, but only if a severity filter allows it (Off is one of the options and the default), and a core command sets the filter. `Ev` has its own step at the top of `core.tick` that flushes that queue into the topic, so telemetry sees an event one tick after it was sent (accepted: events are also in the log). The log queue stays the core's own, filled at send time, and is the burst absorber for the log writer.
- `ctx.ev` is a small handle around `Ev` that fills in the module's `source` and the time. A module cannot set either, so a module cannot claim another module's id. (Not written yet: it waits for `ctx`.)
- `time` is `monotonic_us`: microseconds since this boot, from the OSAL's monotonic clock (what modules get from `ctx.monotonic()`). It restarts at every boot (it is not MET: there is no mission epoch) and it never jumps, so ordering within a run is always right. Runs are told apart by the boot event, so records carry no run id.
- Absolute time (TAI) is not in the envelope. The core emits a time-correlation event (monotonic time plus TAI) whenever the offset is set or changes, and the ground converts. Not designed yet.

``` rust
// core-api
enum Severity { Info, Warn, Err }

const EV_TEXT_MAX_BYTES: usize = 64;   // the default: see `config`

struct Event {
    source: u8,         // 0 = core. Filled in by ctx.ev.
    code: u16,
    severity: Severity, // looked up from the code, not chosen by the caller
    monotonic_us: u64,  // filled in by ctx.ev
    text: FixedStr<EV_TEXT_MAX_BYTES>,
}

/// <Module>-types.rs
// #[derive(event::Code)] generates `impl event::Code`, which has:
//   - index() and from_index(): the variant's position (0, 1, 2, ...), and back. Used by Throttle.
//   - a compile error if there are more variants than EV_MAX_CODES (see Throttling).
//   - code(): the value of the variant (`Self::Started as u16`)
//   - severity(): from the `#[severity(..)]` attribute (Info, Warn or Err). A variant without one,
//     or with fields, is a compile error.
#[derive(event::Code)]
#[repr(u16)]
enum FsEv {
    #[severity(Info)]
    Started,
    #[severity(Err)]
    OpenFailed,
}

// <Module> lib.rs
fn do_something(&mut self, ctx: &mut Ctx) {
    ctx.ev.send(FsEv::Started, format_args!("started"));
    ctx.ev.send(FsEv::OpenFailed, format_args!("open failed: {path} ({err})"));
}
```

### Event Logger
- The core keeps an event log on disk: every event emitted through `ctx.ev`, whatever the downlink `filter` says. The downlink may drop or filter events; the log is the on-board record.
- It is a core feature, not a module. It sees events as they pass through the event service, so it needs no bus queue of its own.
- The log is an append-only binary file of fixed-size records, using the same ideas as the reset log: fixed layout, explicit field-by-field encode and decode, CRC-16/CCITT-FALSE from `core-api`.
- A record is the whole `Event` envelope plus the CRC, 78 bytes with `EV_TEXT_MAX_BYTES` at its default of 64: `monotonic_us` (8), source (1), code (2), severity (1), text (64), CRC (2), all big-endian. Severity is stored even though it can be looked up from the code, so the log can be read without the module's types crate. Decode rejects a bad CRC, an unknown severity, and text that is not valid `FixedStr` content (a non-ASCII byte, non-zero padding).
- There is no run id in a record. The reset report event is the first event of every run and carries the boot count, so a reader splits the log into runs at those events.
- Writes during the tick loop must not block, so the log writer (a core client with its own file service, limit 1) sends the events to the file service in BATCHES (decided 2026-11-20, replacing an earlier plan for a dedicated log thread). Each tick, if its service is idle, it builds one batch: it peeks at the next event in `Ev`'s log queue, encodes the record, and adds it to the request's `Data` with `add_bytes`; the first time a record does not fit, the batch is full. It pops an event only after `add_bytes` accepted it, so nothing is lost, and the batch size is whatever `FILE_DATA_MAX_BYTES` allows, with no number of its own. It then submits one `Append`, and takes the response on a later tick before it builds the next batch. When its service is busy it leaves the events in the log queue, which is where a burst or a stalled disk is absorbed. There is one sync per batch: at low rates a batch is usually one event, so it is one write and one sync per event, and at high rates the batch grows by itself, so the sync is shared exactly when the load is high. This is a group commit by tick, and the durability it gives up is small: a process crash loses nothing either way (written data is in the kernel), and a power cut loses at most the events of about one tick plus the batch in flight. A record that could not fit in an empty batch could never be sent, so the build fails if the record length exceeds `FILE_DATA_MAX_BYTES`. A response that is an error is counted and logged to the system log once per streak, and the batch is lost (there is nothing else to do with it).
- Events emitted before the tick loop starts (`init`, the reset log report) are written with blocking OSAL calls, the same way the reset log is. The loop has not started, so there is nothing to disturb.
- Size cap: checked at startup, like the reset log. At or over the cap (`EVENT_LOG_MAX_RECORDS`, a build-time setting, default 10000 records, at most 1000000), the log is renamed to `.1` and a new file starts.
- A partial last record (crash mid-append) is handled at startup like the reset log's: the whole records are kept, the log is renamed to `.1` (appending after a partial record would shift every later record), and the partial record is reported as an event.
- A panic or abort loses whatever the file service had queued but not yet written. That is accepted; the log is for after-the-fact analysis, not a guaranteed record of the final moments.
- The file side is `core::event_log`: `start` (rotate if full or ending in a partial record, and report a partial record) and `append` (one event, one record), both blocking through the OSAL `Files`. `append` is what the log's worker thread runs for each event. The main thread never calls it in the tick loop. Not written yet.
- The logger only writes. Reading the log (to downlink it, or to show it) is done by ground tooling, or by file transfer to ground (the CFDP module, see File Service). The reader may read the file while the logger appends to it: a partial last record is already tolerated.
- Not decided yet: whether the log is one file or one per source.

## System Log
Free-form text for developers, and for failures the event system cannot report (an event log that will not open, say). Code you cannot see working is hard to trust, so this is how Vee shows what it is doing, in SIL testing and on a real deployment. It is the lowest level of output Vee makes: real events are the first approach, and the system log is downlinked only as a contingency.
- It is the `log` crate's facade (a dependency with no default features, so it works without std). Any crate writes `log::info!("...")`; the logger behind it is `osal::std_syslog`, installed by `std_syslog::init(clock, level)`.
- This is the one exception to "all I/O through `ctx`". A module's data goes through `ctx` because that data is typed and the module needs its types. A log line is only text, and `log::` is a static interface.
- `init` is the first thing `main` does, before anything can fail: a `log::` call made before it is thrown away with no sign. It shares the executive's `StdClock` origin, so a line's time and an event's `monotonic_us` count from the same start.
- Level: set by `init`, default `Info`. A message below the level costs one atomic load and a branch. It is not compiled out (the `log` crate's `max_level_*` features stay available but unused), because it is a debugging tool worth having in flight, and it is low risk.
- A line is `<time_us> <LEVEL> <target>: <message>` (`osal::syslog::Line`, which implements `Display`, so the same line goes to stderr, a buffer or a test comparison without allocating). A finished line is a `syslog::LineBuf`: a fixed 256 byte buffer holding the text (at most 254 bytes, cut at a whole character), then `~` if it was cut, then a newline. Both modes below format into one first.
- The push into any queue must NEVER wait. This is why there are two modes:
  - Direct (written): the calling thread writes the line to stderr. A blocking write is acceptable before startup completes, because it is the only way to see a very early startup failure. A write error is ignored on purpose (there is nowhere else to report it; `eprintln!` would panic).
  - Queued (`std_syslog::start_queue`; the queue and the writer thread are the OSAL's shared `channel` and `thread`, and the queue is never closed: the logger holds its sending end for the life of the process, so the writer never looks for the end): the logger switches to a bounded queue (`QUEUE_DEPTH` lines, 1024, the same as the event queue, so a burst that fills the event queue does not overflow this one; 256 KiB at 256 bytes a line). The calling thread formats the line and does a `try_send`, dropping and counting when it is full. A writer thread polls with `try_recv` and a 10 ms sleep, never a blocking `recv`: measured, a blocked consumer makes each producer's send cost about 18 times more (the wake path takes a lock and a futex call). Each pass writes at most one queue full of lines, then writes a warning line with the number of lines dropped since the last pass, straight to the output (not through the queue, which may be full).
  - The executive's start sequence is one function that calls `start_queue` and then sets `started`, so the queue cannot be forgotten, and nothing else sets `started`. Starting the thread allocates, so it comes first. If it fails (`StartError::Thread`: the OS refused a thread, from a limit on threads or on memory) the logger stays in direct mode and the executive reports it with `log::error!` (which prints directly) and an event. `NotInstalled` (no `init`) and `AlreadyStarted` are bugs in the start sequence and are reported the same way.
- Events are also written to the system log, by `Ev::send`, when the event is sent: after the throttle and before the queue. So the log shows an event the full queue drops (the log is what was sent after throttling, and the event log is what was kept), and the lines come in order with the rest of the log, not in a block when the queue is drained. The line is `<time> <LEVEL> event: src=<source> code=<code> t=<monotonic_us> us: <text>`, with the level from the severity (Info is INFO, Warn is WARN, Err is ERROR). The event's own time is in the line because the line's time is when it was written. They are only written; the log is not downlinked unless needed. The log's queue (256 lines) is smaller than the event queue (1024), so a burst of more than 256 events at once can overflow it. The writer then reports how many lines were dropped.
- A panic with `panic = abort` loses the lines still in the queue. Accepted for now. The panic hook writes the panic's location, thread and message as one line to stderr (see Faults), and where stderr is kept is the deployment's job (a plain text file by systemd, see Faults). Considered and dropped on 2026-11-19: an in-memory ring of recent lines dumped by the hook, and a last gasp file of Vee's own: a ring costs code, and the file on disk already has the lines.
- No capture backend for unit tests: the benefit is for real deployments and SIL runs, not unit tests. Test programs install the logger only if asked (see `core/tests/exec_loop.rs`: `VEE_TEST_LOG=debug cargo test -p vee-core --test exec_loop -- --test-threads=1`).

## OSAL Requirements
- The OSAL is a thin wrapper around std/POSIX calls. It assumes a std implementation exists on the target.
- The OSAL is the only flight crate where std and unsafe are allowed.
- Each OSAL service is split into an interface file, with the traits and plain types, using only `core` (`clock`, `files`, `syslog`), and one file for each implementation, named for its platform (`std_clock`, `std_files`, `std_syslog`) and kept in a directory for that platform (`osal/src/linux/`) or for a test double (`fake_clock`, `fake_files`). The rest of Vee uses the interface, and a new platform adds files without changing the interface. New services follow this.
- Two OSAL files are neither an interface nor a platform's implementation: `channel` (a bounded queue between threads: `try_send`, `try_recv` and a waiting `recv`) and `thread` (start a thread, with the failure as a value). They are concrete wrappers over `std`, shared by every platform that has std, which is the OSAL's contract; they are not Linux specific and nothing implements them twice, so they have no trait. A port to a target without std would change those two files (and could add a trait then, which would need generic associated types for the message type). Decided 2026-11-20.
- It provides what the rest of the code cannot: threads (with priority and CPU pinning), locks, cross-thread queues, device I/O (uart, udp, files), the monotonic clock behind `ctx.monotonic()`, the system log (`syslog`, the logger behind the `log` crate, which is the OSAL's one dependency), and the panic handler.

## Module Requirements
- Code is split between the Core, and Modules. Each module is its own crate, under `modules/` (`modules/example` is the sample), and depends only on `vee-core-api`. The per module bookkeeping the executive keeps (the module, its throttle, its source id and its rate) is one generic `vee_core::slot::Slot<M>`, so a deployment's executive holds one `Slot` for each module and calls them in order.
- Modules all run in one thread, stepped by the executive in a deterministic order.
- Each module owns its sets of command definitions.
- A module may implement `init(ctx)`, which runs once at startup before the first tick. Use it to load tables, read `ctx.reset` (how the last run ended), and do other setup.
- A module implements `step(ctx)`, which runs at the module's rate. It drains the module's bus queues here.
- A module implements `command()`, which handles commands. The exec calls it as part of that module's step, so a module handles its commands at its own rate, not every tick. Commands wait in the module's command queue until its next step (see Command Routing). So a command can wait up to one period of the module, and a slow module needs a command queue deep enough for what arrives in that time.
- Modules get time from `ctx.monotonic()`: microseconds since this boot. It restarts at every boot and never jumps.
- Modules do all I/O (bus, events, files, devices, time) through `ctx`. The one exception is the system log: a module may call `log::info!` and the like directly (see System Log).
- `Module` (core-api) is the trait a module implements: `type Event` (its own event enum), `init(&mut self, ctx)` and `step(&mut self, ctx)`. `command()` is not in the trait yet.
- `Ctx<'a, M>` (core-api) is generic over the module `M`, and what is inside comes from what `M` declares. The executive builds one for every call into a module and passes `&mut Ctx`; the module uses it for that call only. It holds pieces the core owns, which the module borrows: a module never holds a reference to core state of its own. Today it has `ctx.ev` (an `EvHandle<M::Event>`), `ctx.monotonic()`, `ctx.reset`, and the bus handles `ctx.inputs` (`M::In<'a>`) and `ctx.out` (`M::Out<'a>`), which the module declares as generic associated types on `Module` (see [bus.md](bus.md)); files, tables and devices will be added, and the exec config (`uses`, `reads`) will decide which of them each module's `Ctx` gets. A module may only reach core types, its own types and what the exec config connects.
- `ctx.ev.send(code, text)` takes only the module's own event enum (`M::Event`), so the compiler stops a module from sending another module's events. The handle's fields are private and it has only `send`: a module cannot read events or set its own source. The module's source and the time are fixed by the executive. `send` checks the module's `Throttle` first; an event over the limit is not queued.
- The time is read from the clock once by the executive, before each call, and fixed into the `Ctx`: `ctx.monotonic()` and the time of every event sent in that call are the same. There is no clock inside `Ctx`.
- Rust visibility stops at the crate, so everything `pub` in core-api is visible to a module (it depends on core-api). That is harmless: a module has no way to get at the core's instances (the event queue, the throttle) except through its `Ctx`. If an item ever becomes risky to expose, it moves to a crate that modules do not depend on.

``` rust
// fs module
fn command(&mut self, cmd: FsCmd, ctx: &mut Ctx, r: CmdRef) -> CmdResult {
    match cmd {
        FsCmd::Delete(c) => {
            let tag = self.alloc_tag(r)?;                       // fixed array: tag -> CmdRef
            ctx.file.request(FileReq { tag, op: FileOp::Delete(c.path) })
                .map_err(|_| Nak::busy())?;                     // queue full -> immediate NAK
            Ok(Done::Pending)                                   // completion comes later
        }
        /* ... */
    }
}
```

## Executive Requirements
- The executive is constructed from an `exec!` macro for the specific deployment.
- The executive sets up the main loop, run rates and order, and interface threads (for the filesystem and blocking I/O, as defined in the deployment)
- The executive runs at a defined tick rate (100Hz, for example)
    - Ticks start at multiples of the period on the monotonic clock, so a tick has a number: its start time divided by the period. A module steps on the ticks whose number its period divides. The loop waits for each boundary with `Clock::sleep_until_us` (the one blocking call in the loop, in the OSAL, and a fake in tests that just moves the time). After a tick, the next one starts at the first boundary after the clock now, so an overrun skips the boundaries it missed (see the overrun bullet below) and the numbers keep matching wall time.
    - The router puts arriving commands into the modules' command queues in the core's tick. Each module handles its queued commands in its own step, at its own rate. (In the example below, `dispatch` is the time budget for that command handling, now part of the step.)
    - Modules run at a defined, fixed rate, which must divide the base rate. (the major frame is the least common multiple of these periods)
    - Changing rates at runtime is a possible future feature. A module that needs an internal rate (every n steps) makes it table driven.
- Startup order, in the exec's one start function (so a step cannot be forgotten):
    1. `std_syslog::init`, first thing in `main`, before anything can fail.
    2. `Core::start(files, reset_log_path, event_log_path, now)`: the reset log (works out how the last run ended, records this boot, sends the reset report and any problems) and then the event log (rotates it if full or partial, reports a partial record). The core keeps the result: `core.reset()` is what modules see as `ctx.reset`.
    3. Each module's `init`, once, in order, with the time read just before each call.
    4. `std_syslog::start_queue()`, then `std_alloc_guard::arm()` (the `started` flag: arming the allocation check). Starting the queue's thread allocates, so it comes before `started`. (A new thread also allocates in its own startup, after `spawn` has returned, which can land after `arm`. The allocation guard caught this on 2026-11-20. The fix is in the guard, not in the code that starts the thread: `arm()` waits 100 ms first. The guard is a development check, and the code it checks does not change to suit it.) If it fails, the logger stays in direct mode and the exec reports it.
    5. The first tick.
- Shutdown, after the last tick: `Core::stop(now, reason)`. It records a clean shutdown in the reset log (`reset_log::stop`, so the next boot sees `Clean(reason)` and not `Unknown`), then writes every queued event to the event log, which is two `tick`s: the first writes the queue and queues the report of dropped events, if any, and the second writes that report. If the reset log cannot be written, it sends `ResetLogUnavailable`, as startup does, and the final ticks write that event too. A panic, a kill or a power loss never reaches `stop`, which is how a run ends `Unknown`.
- The supervisor watchdog is an optional, low level feature, and the core is not built around it (decided 2026-11-19). Its whole footprint in the core is one `feed()` call at the end of `tick` and one event per failure streak. Nothing else depends on it: no startup call, no check of the tick period against its timeout. If it cannot be set up (`StdWatchdog::from_env` fails: a malformed variable, or no socket), startup carries on with `StdWatchdog::disabled`, which never sends, and the executive reports the error once (`log::error!` and a `WatchdogUnavailable` event). If systemd was in fact waiting for keep-alives, it restarts the process, which is what the watchdog is for.
- The executive logs how long startup took (from the first line of its start function to just before the first tick), so the margin that `WatchdogSec=` needs is easy to check.
- There is no signal handler. systemd stops a service with SIGTERM, which ends Vee without `Core::stop`, so the next boot sees `Unknown`. Accepted 2026-11-19: systemd's own log records that it stopped the service, and a handler needs `unsafe` or a dependency. A clean stop comes from a commanded shutdown (not written) or a test.
- Startup does not fail. If the reset log or the event log cannot be opened, the core carries on without it and sends an Err event that says so (`ResetLogUnavailable`, `EventLogUnavailable`, with the file error). With the reset log down, modules get `ResetInfo::first_boot()`, so a module that reacts to `consecutive_unclean` cannot see a reset loop until the log works again. A broken disk is not a reason to stop a flight computer.
- The exec cannot preempt a module. An overrun is detected and reported (an event after `notify_after` in a row), and nothing is shed: no module is skipped to catch up. Decided 2026-11-19: shedding is a feature with unintended consequences (a command ingest module could be skipped by accident), and a late tick is an acceptable bug, better than lost functionality. A module that never returns is caught by the watchdog. After an overrun the loop SKIPS to the next boundary: the ticks that were missed are not run to catch up, and the overrun event says how many were skipped. Decided 2026-11-19: catching up would make a burst of back to back ticks, and skipping is simpler and bounded.
- Stretch goal, after everything else works: a threaded module type for slow work that deserves a thread of its own or another core (image processing, for example). It would talk to the rest of the system through bounded queues, as the file service does, so the single-threaded executive stays deterministic and testable. (A second executive on another core, with its own modules and queues between the executives, is another way to get there.) Until then, slow work is cut into steps that return, or goes to an interface thread.

### Faults
- Panics abort the process in flight builds (`panic = "abort"`: no unwinding and no recovery). Abort is fail-stop: a panic in any thread, including the file service and the system log writer, stops the whole process, so a supervisor restarts it. With unwinding, a panic in one of those threads would leave the process running without it, and a panic holding a lock would leave half-updated state. Unwinding is for development and test builds only (tests need it, and `#[should_panic]` too). No module fault isolation (`catch_unwind` around a module's `step`) is planned: that is a decision for when `exec!` is designed.
- A panic is the last resort, for when there is truly no alternative. Whenever there is a way to handle an error, Vee handles it and carries on, and reports it as an event. For example, an event whose text is too long is still sent, with the text cut off, because the ground log must see it. Flight code does not `unwrap` or `expect` to skip an error case.
- The panic handler lives in the OSAL. It only reports the panic location (a plain write to stderr) and then aborts. It does not touch the reset log, so a panic leaves the last run as `Unknown`.
- A watchdog trips if the main loop stops ticking. A trip aborts the process, the same as a panic.
- Keeping stderr (the system log, the panic hook's line, anything the process prints) is the deployment's job, not Vee's. On systemd, the sample deployment's unit sends it to a plain text file: `StandardError=append:/var/log/vee/system.log` (systemd 240 or newer; `append:` and not `file:`, which truncates the file at every start and would erase the previous run's tail), with a logrotate rule that caps its size (`copytruncate`, because systemd keeps the file open and a rename would leave it writing to the old one). The journal is not an option for reading back: it is a binary format that only `journalctl` or the sd-journal library can parse. The core does not read the file at boot. After a run that ended `Unknown`, ground sees it in the reset report event and asks for the tail with the file-dump command (through the file service, not written yet). On a platform without systemd the deployment does the equivalent. A portable fallback, if ever wanted: the system log's writer thread appends to its own capped file through `Files`, as the event log does.
- The panic hook (OSAL, flight builds only: `cfg(panic = "abort")`) runs before the abort, so it is where evidence is saved (the "last gasp": the panic's location, thread and message, as ONE system log line on stderr, in the same format and clock as the rest of the log, written with a single write). Decided 2026-11-19: no file of its own, no next boot step, and no dump of recent system log lines (so no in-memory ring). The location, thread and message are what matter most. Where stderr ends up is the deployment's business (see the next bullet). Rules for it, because it runs in an unknown state:
    - It is installed first thing in `main`, next to the system log's `init`. A panic before that runs the default hook, with no last gasp.
    - Its first act is to set a static `PANICKING` flag (a plain store). There is no latch: if two threads panic at once, both run the hook, each writes its line in one write (so the lines do not mix), and whichever finishes first aborts the process, so the other's line may be missing. That is acceptable: simultaneous panics are usually one cause and its consequence, and the line that is written is a valid report. Decided 2026-11-19: a latch that made the others park would also stop a second thread from aborting a process whose first hook is stuck.
    - A panic INSIDE the hook, on the same thread, needs no flag: std aborts at once when a thread panics while already panicking ("thread panicked while processing panic. aborting.", SIGABRT), in `panic = "abort"` and unwinding builds alike. Checked with a throwaway program on 2026-11-19. The line is cut short, but the process dies and is restarted, which is the right outcome.
    - It does not allocate, does not wait for a lock (`try_lock` and give up; the one exception is the lock on stderr that std takes for the single write: another thread holds it only for its own write, and if that write is stuck the hook's would be stuck too, which the `PANICKING` flag and the watchdog already cover), does not touch the queues the main thread owns (it reads only the panic info and a few atomics), writes once and a bounded amount, and never panics. It never touches the reset log, so the run stays `Unknown`.
    - The executive feeds the process watchdog (systemd's `WatchdogSec`, by `sd_notify("WATCHDOG=1")`, through a `Watchdog` trait in the OSAL, owned by Health) only while `PANICKING` is not set. So a hook stuck on a non-main thread, where the main loop would otherwise keep feeding, stops the feed, and the supervisor restarts the process.
    - No `READY=1`, no `Type=notify` (decided 2026-11-19): the unit is the default `Type=simple`, which systemd counts as started as soon as the program runs, and its watchdog timer starts then. `WatchdogSec=` makes `NotifyAccess=` default to `main`, so our keep-alives are accepted. The consequence is that `WatchdogSec=` must be longer than the worst case startup (the logs, the module inits, later the table loads), and it is also the time to notice a hang once running. If startup ever grows too long for that, `feed()` can be called between the module inits at no cost, because it rate limits itself. Units that order themselves `After=` Vee do not wait for its inits.
    - The watchdog feed (`Watchdog` trait in the OSAL: one method, `feed()`, which returns a `Result`): the core calls `feed()` in every `Core::tick`, and the implementation sends a keep-alive on every call, on a non-blocking socket (measured: about 2.5 microseconds a send, so 0.025% of a core at 100 Hz), and withholds it while `std_panic::is_panicking()`. There is no rate limit and no clock in the watchdog (decided 2026-11-19: it would only save those microseconds, and a failed send is simply retried on the next tick). `WATCHDOG_USEC` is read only to tell whether a watchdog exists. A failure is reported once per streak as an Err event, and feeding is tried again. No watchdog configured (no `NOTIFY_SOCKET` or `WATCHDOG_USEC`, as in development) is not an error: `feed()` does nothing.
    - Rebooting after repeated failure: in the sample deployment's unit (`sample-deployment/deploy/vee-sample.service`, with `vee-sample.logrotate` next to it), `StartLimitIntervalSec=`, `StartLimitBurst=N` and `StartLimitAction=reboot-force`, with `Restart=on-failure`. Every restart counts as a start. systemd's counter does not survive a reboot, so a persistent fault loops: the reset log's `consecutive_unclean` is how a module breaks that loop.
    - A hardware watchdog is not designed. It is board specific, so it is a later problem for a mission (likely a module over an OSAL device handle that withholds the feed while `std_panic::is_panicking()`, so a hung panic hook cannot be hidden by a module that keeps stepping). Decided 2026-11-19: not now.
    - A write that blocks on failing storage can sit in uninterruptible sleep, which not even a kill ends until the I/O returns. That is a limit of the OS. The hook's write stays small: one line, one write, to stderr.
    - The hook is thin: the logic (building the line) is in functions tests can call. The hook itself gets one end-to-end check with a child process.
- Vee does nothing beyond aborting. Restarting the process is up to the supervisor (for example systemd), including detecting a reset loop and rebooting the system. The reset log's `consecutive_unclean` is there for modules that want to react to a loop on their own.
- Vee itself has no modes. If a mission needs modes (safe mode, for example), a mission module provides them.

``` rust
// Example deployment
exec! {
    base: 100 Hz;
    overrun: { notify_after: 3 };

    devices: [ uart0 = osal::Uart("/dev/ttyS1", 115200), gnd = osal::UdpSocket(5000) ];
    sinks:   [ ground = 1, rts = 2, fdir = 3 ];
    services: [ file: FileSvc { prio: LOW, cpu: 3, depth: 8, resp_depth: 4,
                                clients: [core, ats, recorder], timeout: 200 ticks } ];
    tables:  [ dir: "/data/tables", boot: [ats.ats[0] -> ats_a ] ];
    topics:  [ nav_state: NavState @ 0x101 { writer: survival },        // a state: latest value and a seq
               gnd_cmd:   GndCmd   @ 0x1F0 { writer: link, readers: [ats: 4] } ];  // a queued link; reader: queue depth
    events:  [ out: gnd, filter: Info ];                   // queue depth: the EV_LOG_QUEUE_DEPTH setting

    order: [
        link: Link @ 100 Hz { step: 1 ms, dispatch: 0.1 ms, cmd_depth: 8,
                              sink: ground, devices: [uart0, gnd] },
        core                { budget: 1 ms },
        ats:  Ats  @   1 Hz { step: 2 ms, dispatch: 0.2 ms, cmd_depth: 4,
                              sink: ats, uses: [file], reads: [nav_state] },
        fdir: Fdir @ 10 Hz  { step: 1 ms, dispatch: 0.1 ms, cmd_depth: 4,
                              sink: fdir, reads: all, writes: [fdir_health] },   // all but its own
    ],
}
```

In the above example, note how Ctx is a universal type. Start with that, then we can derive module specific Ctx types later, to enforce device access rules at compile time.

## Build-time settings
- Important sizes and limits are fixed when Vee is built, and a deployment can override them without editing Vee. A setting is declared where it is used (in the core or in any module) with `config::setting!` (`core-api/src/config.rs`): its name, type, default and the range of values that make sense, on one line:
  ``` rust
  config::setting! {
      /// How many bytes of text an event carries.
      pub EV_TEXT_MAX_BYTES: usize = 64, 1..=4096;
  }
  ```
  It writes a constant of that name, and checks the value against the range (the default too).
- The value is read from an environment variable named after the setting with `VEE_` in front (`VEE_EV_TEXT_MAX_BYTES`), with `option_env!` while compiling. A deployment sets it in the `[env]` table of its own `.cargo/config.toml`, in the directory cargo is run from (`sample-deployment/.cargo/config.toml` is the example). Cargo reads config files from where it is run, not from a dependency's directory, so Vee ships no config of its own: its defaults are in the code.
- Where a value comes from, highest first: the real environment, `cargo build --config file.toml`, the nearest `.cargo/config.toml` (then those above it), the default in the code.
- A value that is not a plain whole number, is too big, or is outside its range stops the build with an error that names the setting and points at its declaration. The check runs in a `const`, so it is a compile error and never a panic in flight.
- Two settings with the same name would share one variable, so a module starts its setting names with its own name (`FS_MAX_OPEN`).
- The core, the modules and the ground tool must all be built with the same values, because some are part of the log format (`EV_TEXT_MAX_BYTES` sets the event log record size). Nothing checks this at runtime yet.
- There is no single list of settings. Search for `setting!` to find them all; each shows its default and range. A deployment's config lists the ones it sets.
- To add a setting: declare it where it is used, and add a line to the sample deployment's config.
