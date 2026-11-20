# Vee Requirements
Vee (the Very Economical Engine) is a super lightweight flight software framework written in rust. It targets Linux-class systems (anything where rust's std is available, including many RTOSes).

This repo contains the core, a set of standard modules, and an example mission configuration.

*code samples are pseudocode / examples.*

## General Requirements
- No std or unsafe is enforced everywhere except the OSAL. Host-side tools (ground-tool, proc-macro crates) may use std.
- Static allocation is the goal: every queue, table, and buffer is sized at compile time (const generics, sizes from `exec!`).
- Allocation at startup is tolerated only where static is not practical.
- No allocation after startup. This is a rule for the code, and it is checked in development, not enforced in flight: a global allocator (provided by the OSAL, declared by the deployment's `main`, behind a Cargo feature that dev, SIL and CI builds turn on) aborts with a fixed message on any allocation once the exec sets its `started` flag. A flight build has no guard. An allocation after startup that succeeds is a bug to find before flight, and killing the process for it would turn that bug into an outage.
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
| Health | The reset log (how the last run ended, `ctx.reset`), overrun detection, the watchdog kick, and the core's own counters | `start`, `tick`, `stop` | the reset log exists (`reset_log`) |
| Time | The clock, read once per call and fixed into each `Ctx`; the tick count. Later, the monotonic to TAI correlation event | `tick` | `Clock` in the OSAL |
| Events | The event queue (`Ev`), per event throttling, the report of suppressed events, the event log | `start`, `tick` (drain), `stop` (last drain) | `ev`, `throttle`, `suppressed`, `events` (`report_dropped`), `event_log` (file side), and `Core::tick` taking events out and writing them to the event log (blocking, temporarily) |
| Message Bus | One writer, N readers, a queue per reader | none planned: a send queues, a module drains in `step` | not started |
| Command Router | Commands to a module's `command()`, acks, and a result for the sender's `seq` | `tick` (route into the modules' command queues) | not started |
| Tables | The table sets, the one load buffer and one dump buffer | `start` (load), `tick` (follow file results), `stop` (dump) | not started |
| Files | The file service: a thread, bounded request queues, results back. Clients are Tables, the event log and modules (`ctx.file`) | `start`, `tick` (collect results), `stop` | `Files` in the OSAL (blocking, for startup and shutdown) |

- The reset log is in Health because it is related to it, but it is not a service: nothing calls it during a run. `start` produces `ctx.reset` and `stop` records a clean shutdown. A panic leaves the last run `Unknown` (see Faults).
- The Bus and the Command Router stay separate. The bus is publish and subscribe with a queue per reader. The router is point to point (target and opcode) with acks and results. They share only `queue::Queue`.
- Files is the only domain that owns a thread. Its clients never block: a request goes into a bounded queue (a full queue is an immediate nak or a counted loss) and the answer comes back on a later tick.
- Health reports what it finds as events, so it sends through Events. Events and Tables both send through Files.

The executive schedules the core like a module, every tick and with no divisor (`core { budget: .. }` in `order`), but the core is not a `Module`: it owns `Ev` and the logs, and its own events skip the throttle. The core is one struct, `Core`, that owns the domains and calls their hooks in an order the core fixes, so a deployment cannot get it wrong. It has one entry point per tick, `core.tick(now)`, and no begin and end split: the loop comes back around, so draining events, the watchdog kick and overrun detection (compare the start times of consecutive ticks) can happen at the top of the next tick. Start and shutdown are separate calls, `start` and `stop`. `exec!` generates the loop, because only the deployment knows every module type (`Module` has an associated type, so no list of modules fits in `core`). Each tick:

1. For each module, report its suppressed events (`suppressed::report`, generic over the module's event enum). This also zeroes that module's throttle, which starts the tick's count.
2. `core.tick(now)`.
3. For each module due this tick, read the clock, build its `Ctx` and call `step`. The module's queued commands are handled in this step (see Command Routing), not every tick.

`Core` (`vee_core::Core<F: Files>`, in `core/src/lib.rs`) exists with the Events domain and Health's reset info. `Core::start` and `Core::stop` are its startup and shutdown (see Executive Requirements). It owns the event queue (`pub ev`, which the executive lends to every `Ctx`), the file system `F` and the event log path. Its `tick(now)` does: (1) take every event out of the queue and append it to the event log, then (2) `events::report_dropped`, in that order, so the report has room in the queue. Until the file service exists the append is a blocking write in the tick, which the design forbids in the loop: it is temporary, and the file service will replace the `append` call with a request, leaving the loop as it is. A failed append loses that event (nothing else can be done with it) but the queue is still emptied, and the failures are reported once per tick to the system log. The events of a tick's modules are written in the next tick, and the suppressed reports in the same one. Not written yet: `exec!`, the other domains inside `core.tick` (their order is decided as each is written), and the Bus, Router, Tables and Files domains.

### Software Bus
- The core provides a typed software bus between modules.
- Types are defined in each module, in a dedicated types crate that other modules can use to learn about those types (to avoid a dependency loop between modules).
- The software bus uses a one-writer, N-reader paradigm.
- Every reader gets its own fixed-depth queue per message type, so a slow reader never loses data because of a fast one. Depths are set in `exec!`.
- A module drains its queues in `step()`, in whatever order it chooses.
- Queues on the main thread (the bus, command queues, the event queue) are one generic type, `core_api::queue::Queue<T, N>`: a fixed-size first-in first-out queue with no allocation. `push` gives the item back when the queue is full, so each queue chooses its own policy (drop the newest, drop the oldest, or nak). Queues that cross threads belong to the OSAL.
- A full queue is an error that is reported as an event, never a silent drop. (Which message is dropped, oldest or newest, is chosen per queue in `exec!`.)

### Command Routing
- The core provides for command routing to modules.
- Each module, and the core, define their own commands. Modules define these in their types crate.
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
- The event service is `core_api::ev::Ev` (in core-api, not the core, so `Ctx` can hold a reference to it): a `core_api::queue::Queue<Event, EV_QUEUE_DEPTH>` plus a count of dropped events, with no I/O of its own (only the system log write below) and no clock. There is one event queue in the whole system, so its depth is a build-time setting (`EV_QUEUE_DEPTH`, default 1024, which is 80 KiB at 80 bytes a slot) and not a type parameter. There is no cap per source, only the per event throttle, so the worst case in one tick is every source sending each of its events `EV_MAX_PER_TICK` times: size the queue for that. `send(source, monotonic_us, code, text)` builds the `Event`, writes it to the system log, and queues it (see System Log: the write comes first, so an event the queue cannot take is still seen). The event log and the downlink take events out with `pop`, oldest first. `send` never fails, because the sender can do nothing about a failure: text that is too long is cut off and marked (see above), a value in the text that fails to print leaves the text up to that point, and a full queue drops the new event (the oldest are kept, because the first events of a problem are the most useful) and counts it. The core takes that count with `take_dropped` once per tick, after it has emptied the queue, and sends it on as one `CoreEvent::EventsDropped` (Err, code 7, text like `37 events dropped: the queue was full`): `core::events::report_dropped`. Only the count is kept; the dropped events themselves are the newest of the burst, and the system log has them. Modules never get an `Ev`: they only get the `ctx.ev` handle, which can only send.
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
- Writes during the tick loop must not block, so they go through the file service (a bounded request queue, like any other client). If that queue is full, the event is not written. The logger counts the missed events, and the next record that does get written carries the count (as an ordinary event, so the loss is never silent).
- Events emitted before the tick loop starts (`init`, the reset log report) are written with blocking OSAL calls, the same way the reset log is. The loop has not started, so there is nothing to disturb.
- Size cap: checked at startup, like the reset log. At or over the cap (`EVENT_LOG_MAX_RECORDS`, a build-time setting, default 10000 records, at most 1000000), the log is renamed to `.1` and a new file starts.
- A partial last record (crash mid-append) is handled at startup like the reset log's: the whole records are kept, the log is renamed to `.1` (appending after a partial record would shift every later record), and the partial record is reported as an event.
- A panic or abort loses whatever the file service had queued but not yet written. That is accepted; the log is for after-the-fact analysis, not a guaranteed record of the final moments.
- The file side is `core::event_log`: `start` (rotate if full or ending in a partial record, and report a partial record) and `append` (one event, one record), both blocking through the OSAL `Files`. `append` is what the file service's thread will run. The main thread never calls it in the tick loop: it will encode events and hand them to the file service, which may be a thread of its own for the log. Not written yet.
- The logger only writes. Reading the log (to downlink it, or to show it) is done by ground tooling, or by a later file-dump command through the file service.
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
  - Queued (`std_syslog::start_queue`): the logger switches to a bounded queue (`QUEUE_DEPTH` lines, 256). The calling thread formats the line and does a `try_send`, dropping and counting when it is full. A writer thread polls with `try_recv` and a 10 ms sleep, never a blocking `recv`: measured, a blocked consumer makes each producer's send cost about 18 times more (the wake path takes a lock and a futex call). Each pass writes at most one queue full of lines, then writes a warning line with the number of lines dropped since the last pass, straight to the output (not through the queue, which may be full).
  - The executive's start sequence is one function that calls `start_queue` and then sets `started`, so the queue cannot be forgotten, and nothing else sets `started`. Starting the thread allocates, so it comes first. If it fails (`StartError::Thread`: the OS refused a thread, from a limit on threads or on memory) the logger stays in direct mode and the executive reports it with `log::error!` (which prints directly) and an event. `NotInstalled` (no `init`) and `AlreadyStarted` are bugs in the start sequence and are reported the same way.
- Events are also written to the system log, by `Ev::send`, when the event is sent: after the throttle and before the queue. So the log shows an event the full queue drops (the log is what was sent after throttling, and the event log is what was kept), and the lines come in order with the rest of the log, not in a block when the queue is drained. The line is `<time> <LEVEL> event: src=<source> code=<code> t=<monotonic_us> us: <text>`, with the level from the severity (Info is INFO, Warn is WARN, Err is ERROR). The event's own time is in the line because the line's time is when it was written. They are only written; the log is not downlinked unless needed. The log's queue (256 lines) is smaller than the event queue (1024), so a burst of more than 256 events at once can overflow it. The writer then reports how many lines were dropped.
- A panic with `panic = abort` loses the lines still in the queue. Accepted for now. Later options: an mmap'd ring file, or a persisted file plus an in-memory ring of recent lines that a command can dump (like cFS's system log).
- No capture backend for unit tests: the benefit is for real deployments and SIL runs, not unit tests. Test programs install the logger only if asked (see `core/tests/exec_loop.rs`: `VEE_TEST_LOG=debug cargo test -p vee-core --test exec_loop -- --test-threads=1`).

## OSAL Requirements
- The OSAL is a thin wrapper around std/POSIX calls. It assumes a std implementation exists on the target.
- The OSAL is the only flight crate where std and unsafe are allowed.
- Each OSAL service is split into an interface file, with the traits and plain types, using only `core` (`clock`, `files`, `syslog`), and one file for each implementation, named for its platform (`std_clock`, `std_files`, `std_syslog`) or for a test double (`fake_clock`, `fake_files`). The rest of Vee uses the interface, and a new platform adds files without changing the interface. New services follow this.
- It provides what the rest of the code cannot: threads (with priority and CPU pinning), locks, cross-thread queues, device I/O (uart, udp, files), the monotonic clock behind `ctx.monotonic()`, the system log (`syslog`, the logger behind the `log` crate, which is the OSAL's one dependency), and the panic handler.

## Module Requirements
- Code is split between the Core, and Modules. Each module is its own crate.
- Modules all run in one thread, stepped by the executive in a deterministic order.
- Each module owns its sets of command definitions.
- A module may implement `init(ctx)`, which runs once at startup before the first tick. Use it to load tables, read `ctx.reset` (how the last run ended), and do other setup.
- A module implements `step(ctx)`, which runs at the module's rate. It drains the module's bus queues here.
- A module implements `command()`, which handles commands. The exec calls it as part of that module's step, so a module handles its commands at its own rate, not every tick. Commands wait in the module's command queue until its next step (see Command Routing). So a command can wait up to one period of the module, and a slow module needs a command queue deep enough for what arrives in that time.
- Modules get time from `ctx.monotonic()`: microseconds since this boot. It restarts at every boot and never jumps.
- Modules do all I/O (bus, events, files, devices, time) through `ctx`. The one exception is the system log: a module may call `log::info!` and the like directly (see System Log).
- `Module` (core-api) is the trait a module implements: `type Event` (its own event enum), `init(&mut self, ctx)` and `step(&mut self, ctx)`. `command()` is not in the trait yet.
- `Ctx<'a, M>` (core-api) is generic over the module `M`, and what is inside comes from what `M` declares. The executive builds one for every call into a module and passes `&mut Ctx`; the module uses it for that call only. It holds pieces the core owns, which the module borrows: a module never holds a reference to core state of its own. Today it has `ctx.ev` (an `EvHandle<M::Event>`), `ctx.monotonic()` and `ctx.reset`; the bus, files, tables and devices will be added, and the exec config (`uses`, `reads`) will decide which of them each module's `Ctx` gets. A module may only reach core types, its own types and what the exec config connects.
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
    - The router puts arriving commands into the modules' command queues in the core's tick. Each module handles its queued commands in its own step, at its own rate. (In the example below, `dispatch` is the time budget for that command handling, now part of the step.)
    - Modules run at a defined, fixed rate, which must divide the base rate. (the major frame is the least common multiple of these periods)
    - Changing rates at runtime is a possible future feature. A module that needs an internal rate (every n steps) makes it table driven.
- Startup order, in the exec's one start function (so a step cannot be forgotten):
    1. `std_syslog::init`, first thing in `main`, before anything can fail.
    2. `Core::start(files, reset_log_path, event_log_path, now)`: the reset log (works out how the last run ended, records this boot, sends the reset report and any problems) and then the event log (rotates it if full or partial, reports a partial record). The core keeps the result: `core.reset()` is what modules see as `ctx.reset`.
    3. Each module's `init`, once, in order, with the time read just before each call.
    4. `std_syslog::start_queue()`, then the `started` flag (arming the allocation check). Starting the queue's thread allocates, so it comes before `started`. If it fails, the logger stays in direct mode and the exec reports it.
    5. The first tick.
- Shutdown, after the last tick: `Core::stop(now, reason)`. It records a clean shutdown in the reset log (`reset_log::stop`, so the next boot sees `Clean(reason)` and not `Unknown`), then writes every queued event to the event log, which is two `tick`s: the first writes the queue and queues the report of dropped events, if any, and the second writes that report. If the reset log cannot be written, it sends `ResetLogUnavailable`, as startup does, and the final ticks write that event too. A panic, a kill or a power loss never reaches `stop`, which is how a run ends `Unknown`.
- Startup does not fail. If the reset log or the event log cannot be opened, the core carries on without it and sends an Err event that says so (`ResetLogUnavailable`, `EventLogUnavailable`, with the file error). With the reset log down, modules get `ResetInfo::first_boot()`, so a module that reacts to `consecutive_unclean` cannot see a reset loop until the log works again. A broken disk is not a reason to stop a flight computer.
- The exec cannot preempt a module. An overrun is detected and reported, and `overrun` says what to shed. A module that never returns is caught by the watchdog.
- Stretch goal, after everything else works: a threaded module type for slow work that deserves a thread of its own or another core (image processing, for example). It would talk to the rest of the system through bounded queues, as the file service does, so the single-threaded executive stays deterministic and testable. (A second executive on another core, with its own modules and queues between the executives, is another way to get there.) Until then, slow work is cut into steps that return, or goes to an interface thread.

### Faults
- Panics abort the process in flight builds (`panic = "abort"`: no unwinding and no recovery). Abort is fail-stop: a panic in any thread, including the file service and the system log writer, stops the whole process, so a supervisor restarts it. With unwinding, a panic in one of those threads would leave the process running without it, and a panic holding a lock would leave half-updated state. Unwinding is for development and test builds only (tests need it, and `#[should_panic]` too). No module fault isolation (`catch_unwind` around a module's `step`) is planned: that is a decision for when `exec!` is designed.
- A panic is the last resort, for when there is truly no alternative. Whenever there is a way to handle an error, Vee handles it and carries on, and reports it as an event. For example, an event whose text is too long is still sent, with the text cut off, because the ground log must see it. Flight code does not `unwrap` or `expect` to skip an error case.
- The panic handler lives in the OSAL. It only reports the panic location (a plain write to stderr) and then aborts. It does not touch the reset log, so a panic leaves the last run as `Unknown`.
- A watchdog trips if the main loop stops ticking. A trip aborts the process, the same as a panic.
- Vee does nothing beyond aborting. Restarting the process is up to the supervisor (for example systemd), including detecting a reset loop and rebooting the system. The reset log's `consecutive_unclean` is there for modules that want to react to a loop on their own.
- Vee itself has no modes. If a mission needs modes (safe mode, for example), a mission module provides them.

``` rust
// Example deployment
exec! {
    base: 100 Hz;
    overrun: { notify_after: 3, shed: sheddable_first };

    devices: [ uart0 = osal::Uart("/dev/ttyS1", 115200), gnd = osal::UdpSocket(5000) ];
    sinks:   [ ground = 1, rts = 2, fdir = 3 ];
    services: [ file: FileSvc { prio: LOW, cpu: 3, depth: 8, resp_depth: 4,
                                clients: [core, ats, recorder], timeout: 200 ticks } ];
    tables:  [ dir: "/data/tables", boot: [ats.ats[0] -> ats_a ] ];
    bus:     [ nav_state: NavState { writer: survival, readers: [fdir: 4, ats: 4] } ];  // reader: queue depth
    events:  [ out: gnd, filter: Info ];                   // queue depth: the EV_QUEUE_DEPTH setting

    order: [
        link: Link @ 100 Hz { essential, step: 1 ms, dispatch: 0.1 ms, cmd_depth: 8,
                              sink: ground, devices: [uart0, gnd] },
        core                { budget: 1 ms },
        ats:  Ats  @   1 Hz { sheddable, step: 2 ms, dispatch: 0.2 ms, cmd_depth: 4,
                              sink: ats, uses: [file], reads: [nav_state] },
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
