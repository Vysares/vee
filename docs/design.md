# Vee Requirements
Vee (the Very Economical Engine) is a super lightweight flight software framework written in rust. It targets Linux-class systems (anything where rust's std is available, including many RTOSes).

This repo contains the core, a set of standard modules, and an example mission configuration.

*code samples are pseudocode / examples.*

## General Requirements
- No std or unsafe is enforced everywhere except the OSAL. Host-side tools (ground-tool, proc-macro crates) may use std.
- Static allocation is the goal: every queue, table, and buffer is sized at compile time (const generics, sizes from `exec!`).
- Allocation at startup is tolerated only where static is not practical.
- No allocation after startup. Enforced by a global allocator (in the OSAL) that panics on any allocation once the exec sets its `started` flag.
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

### Software Bus
- The core provides a typed software bus between modules.
- Types are defined in each module, in a dedicated types crate that other modules can use to learn about those types (to avoid a dependency loop between modules).
- The software bus uses a one-writer, N-reader paradigm.
- Every reader gets its own fixed-depth queue per message type, so a slow reader never loses data because of a fast one. Depths are set in `exec!`.
- A module drains its queues in `step()`, in whatever order it chooses.
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
    4. The core makes the result available to modules as `ctx.reset` (see below).
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
- Every event has the same envelope: source (the module), code, severity, a monotonic time, and text of a fixed size (`EV_TEXT_MAX_BYTES`, 50 by default, set at build time: see Build-time settings). Every event is the same size on the wire and on disk: shorter text is zero padded.
- The text is a `FixedStr<EV_TEXT_MAX_BYTES>` (`core-api`): ASCII only, exactly that many bytes on the wire, zero padded, ending at the first zero byte. Text that is too long is cut off and the event is still sent, because the ground log must see it (see Faults). A cut-off text ends in `~` (`fixed_str::TRUNCATED_MARK`, replacing the last kept byte), so a reader can see it was cut. A text of exactly the maximum length is not marked. Writing text with `format_args!` goes through `fmt::Write` on `FixedStr`, which cuts off the same way and never fails: a character that cannot be in the text (non-ASCII) becomes `?`. No allocation happens in flight.
- The code is the value of an enum variant, on an enum marked `#[repr(u16)]`, one enum per source (a module's, or the core's). A code is a `u16`. Values are implicit (0, 1, 2, ... in the order written) and Rust refuses two variants with the same value, so codes are unique within an enum. The catch: a code is then the variant's position, so reordering variants or inserting one in the middle renumbers the codes after it, and old logs would decode wrongly. New events go at the end. A variant may be given an explicit value (`Started = 0x01`) to pin its code.
- Each code has a fixed severity: info, warn, err. It is given once, on the variant, and the caller of `ctx.ev.send` never picks it.
- `#[derive(event::Code)]` (core-derive; the trait is `event::Code`) generates the lookup (below). The core's own events use it exactly as a module's do.
- Command acks and naks are plain events (see Commands). They have no special codes, kind bits or fields. They are written the same way as any other event, with the details in the text.
- `ctx.ev` fills in `source` and `time`. A module cannot set either, so a module cannot claim another module's id.
- `time` is `monotonic_us`: microseconds since this boot, from the OSAL's monotonic clock (what modules get from `ctx.monotonic()`). It restarts at every boot (it is not MET: there is no mission epoch) and it never jumps, so ordering within a run is always right. Runs are told apart by the boot event, so records carry no run id.
- Absolute time (TAI) is not in the envelope. The core emits a time-correlation event (monotonic time plus TAI) whenever the offset is set or changes, and the ground converts. Not designed yet.

``` rust
// core-api
enum Severity { Info, Warn, Err }

const EV_TEXT_MAX_BYTES: usize = 50;   // the default: see `config`

struct Event {
    source: u8,         // 0 = core. Filled in by ctx.ev.
    code: u16,
    severity: Severity, // looked up from the code, not chosen by the caller
    monotonic_us: u64,  // filled in by ctx.ev
    text: FixedStr<EV_TEXT_MAX_BYTES>,
}

/// <Module>-types.rs
// #[derive(event::Code)] generates `impl event::Code`, which has:
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
- A record is the whole `Event` envelope plus the CRC, 64 bytes with `EV_TEXT_MAX_BYTES` at its default of 50: `monotonic_us` (8), source (1), code (2), severity (1), text (50), CRC (2), all big-endian. Severity is stored even though it can be looked up from the code, so the log can be read without the module's types crate. Decode rejects a bad CRC, an unknown severity, and text that is not valid `FixedStr` content (a non-ASCII byte, non-zero padding).
- There is no run id in a record. The reset report event is the first event of every run and carries the boot count, so a reader splits the log into runs at those events.
- Writes during the tick loop must not block, so they go through the file service (a bounded request queue, like any other client). If that queue is full, the event is not written. The logger counts the missed events, and the next record that does get written carries the count (as an ordinary event, so the loss is never silent).
- Events emitted before the tick loop starts (`init`, the reset log report) are written with blocking OSAL calls, the same way the reset log is. The loop has not started, so there is nothing to disturb.
- Size cap: checked at startup, like the reset log. Over the cap, the log is renamed to `.1` and a new file starts. The cap is a named constant for now (see the compile-time settings open question in `progress.md`).
- A partial last record (crash mid-append) is handled at startup like the reset log's: the whole records are kept, the log is renamed to `.1` (appending after a partial record would shift every later record), and the partial record is reported as an event.
- A panic or abort loses whatever the file service had queued but not yet written. That is accepted; the log is for after-the-fact analysis, not a guaranteed record of the final moments.
- The logger only writes. Reading the log (to downlink it, or to show it) is done by ground tooling, or by a later file-dump command through the file service.
- Not decided yet: whether the log is one file or one per source.

## OSAL Requirements
- The OSAL is a thin wrapper around std/POSIX calls. It assumes a std implementation exists on the target.
- The OSAL is the only flight crate where std and unsafe are allowed.
- It provides what the rest of the code cannot: threads (with priority and CPU pinning), locks, cross-thread queues, device I/O (uart, udp, files), the monotonic clock behind `ctx.monotonic()`, and the panic handler.

## Module Requirements
- Code is split between the Core, and Modules. Each module is its own crate.
- Modules all run in one thread, stepped by the executive in a deterministic order.
- Each module owns its sets of command definitions.
- A module may implement `init(ctx)`, which runs once at startup before the first tick. Use it to load tables, read `ctx.reset` (how the last run ended), and do other setup.
- A module implements `step(ctx)`, which runs at the module's rate. It drains the module's bus queues here.
- A module implements `command()`, which handles commands. The exec calls it at the base tick rate, not the module's rate, so commands are not delayed by a slow module.
- Modules get time from `ctx.monotonic()`: microseconds since this boot. It restarts at every boot and never jumps.
- Modules do all I/O (bus, events, files, devices, time) through `ctx`.

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
    - The exec dispatches commands every tick
    - Modules run at a defined, fixed rate, which must divide the base rate. (the major frame is the least common multiple of these periods)
    - Changing rates at runtime is a possible future feature. A module that needs an internal rate (every n steps) makes it table driven.
- The exec has the core run its startup reset log step first (see Reset Log).
- The exec calls each module's `init` once, in order, then sets the `started` flag (arming the allocation check) before the first tick.
- The exec cannot preempt a module. An overrun is detected and reported, and `overrun` says what to shed. A module that never returns is caught by the watchdog.

### Faults
- Panics abort the process. Flight crates build with `panic = "abort"`, so there is no unwinding and no recovery from a panic.
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
    events:  [ out: gnd, depth: 32, filter: Info ];

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
- Important sizes and limits are fixed when Vee is built, and a deployment can override them without editing Vee. Each setting is a constant defined next to the code that uses it, with its default there. `config::parse_or` reads it. `core-api/src/config.rs` also holds the checks on every setting's value (a `const _` assert each), so they can be audited in one place.
- A setting is an environment variable named `VEE_<NAME>`, read with `option_env!` while compiling. A deployment sets it in the `[env]` table of its own `.cargo/config.toml`, in the directory cargo is run from (`sample-deployment/.cargo/config.toml` is the example). Cargo reads config files from where it is run, not from a dependency's directory, so Vee ships no config of its own: its defaults are in the code.
- Where a value comes from, highest first: the real environment, `cargo build --config file.toml`, the nearest `.cargo/config.toml` (then those above it), the default in the code.
- A value that is not a plain whole number, is too big, or breaks a check stops the build with an error that points at the setting. The check runs in a `const`, so it is a compile error and never a panic in flight.
- The core, the modules and the ground tool must all be built with the same values, because some are part of the log format (`EV_TEXT_MAX_BYTES` sets the event log record size). Nothing checks this at runtime yet.
- To add a setting: define it where it is used (in `core-api`, because `config.rs` can only check constants it can see), add its check to `config.rs`, and add a line to the sample deployment's config. Search for `option_env!` to list them all.
