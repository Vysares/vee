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
- Each command sink is one event source. ACK and NAK are each an event code.
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
- Every event has the same envelope: source (the module), code, severity, and a small fixed-size args payload.
- Each event code from a given source has a severity: info, warn, err.
- Args are typed per event code, from a derive. They are fixed-size primitives (and fixed-capacity strings), so no allocation and no text formatting happens in flight.
- Each event code also has a text template, kept in the module's types crate. The ground tool uses it to render the args as text. The text is not sent over the wire.
- Command acks and naks are ordinary events whose args carry the details (sink, command ref, reason).

``` rust
// Core's ev.rs
enum Severity { Info, Warn, Err }

struct Event {
    source: u8,         // 0 = core
    code: u16,
    severity: Severity, // looked up from the code, not sent by the caller
    args: [u8; EV_ARGS_MAX], len: u8,  // encoded by the derive
}

/// <Module>-types.rs
// #[derive(Event)] generates:
//   - per-variant arg encode/decode
//   - a code -> severity lookup, and a compile error if a variant has no severity
//   - a code -> text template lookup, used by ground-tool
#[derive(Event)]
enum FsEv {
    #[event(Severity::Info, "started")]                        Started,
    #[event(Severity::Err,  "open failed: {path} ({err})")]    OpenErr { path: Path, err: u8 },
}

// <Module> lib.rs
fn do_something(&mut self, ctx: &mut Ctx) {
    ctx.ev.send(FsEv::OpenErr { path, err });
}
```

## OSAL Requirements
- The OSAL is a thin wrapper around std/POSIX calls. It assumes a std implementation exists on the target.
- The OSAL is the only flight crate where std and unsafe are allowed.
- It provides what the rest of the code cannot: threads (with priority and CPU pinning), locks, cross-thread queues, device I/O (uart, udp, files), the monotonic clock behind `ctx.now()`, and the panic handler.

## Module Requirements
- Code is split between the Core, and Modules. Each module is its own crate.
- Modules all run in one thread, stepped by the executive in a deterministic order.
- Each module owns its sets of command definitions.
- A module may implement `init(ctx)`, which runs once at startup before the first tick. Use it to load tables, read `ctx.reset` (how the last run ended), and do other setup.
- A module implements `step(ctx)`, which runs at the module's rate. It drains the module's bus queues here.
- A module implements `command()`, which handles commands. The exec calls it at the base tick rate, not the module's rate, so commands are not delayed by a slow module.
- Modules get time from `ctx.now()`.
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
