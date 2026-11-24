# Software Bus

Design notes for the bus, split out of `design.md` on 2026-11-23 and settled with the user the same day.
High level on purpose: bullets and small snippets. NOTHING HERE IS WRITTEN YET. **Settled** means decided
with the user. **Open** means still to decide.

## What the bus is for
- Modules share data without knowing each other's internals: one module produces, others read.
- Publish and subscribe. Commands are NOT on the bus (that is the Command Router, point to point).
- Everything runs on the main thread, one module at a time: no locks, no allocation, no blocking.

## Use cases (what the design must serve)
1. **Telemetry output** reads the latest value of state topics from all modules, choosing by a table loaded at
   runtime.
2. **Telemetry output** also drains queued topics (events, a comms passthrough), filtered by a table.
3. **FDIR** reads state topics from all modules (fields, not bytes), and may read queued ones (events).
4. **Pipelines**: ordinary modules read other modules' state and queued topics, typed, often generic over the
   message type (`Filter<T>`).
5. **Recorder**: reads everything, by bytes.
6. **Fan-in**: several producers feed one consumer (the downlink: telemetry, event forwarding, CFDP, files).
7. **Flow control**: a producer (CFDP) must know when its consumer is full.

Rule for 1, 2, 3 and 5: these modules are wired to every topic they could ever need at compile time. The table
only chooses among topics they already have. Nothing subscribes at runtime.

## Settled
- A **topic** has a name, a message type and an explicit APID (`u16`, also the CCSDS APID on downlink). The APID
  is the identity in tables, the recorder and the dictionary.
- **APIDs are unique and are all defined in one place: the deployment** (the `topics { .. }` block of `exec!`),
  never in a module crate (two instances of one module would collide). The core's topics are in the same list.
  A duplicate is a compile error; before `exec!` exists, the hand written sample keeps an `apids` module and a
  `const fn` check. There is no range check on the `u16` for now (a CCSDS APID is 11 bits, 0x7FF reserved for idle
  packets; revisit when a deployment downlinks CCSDS). The ground dictionary
  is generated from the same list.
- A message is `Copy` plain data (up to about 3 kB). `#[derive(bus::Message)]` writes both the wire encoding (big
  endian, declaration order) and **field access** (see Messages). A field's index is its position in that layout.
- **One writer per topic.** The core is a writer like any module (the events topic, time correlation). Its topics
  are declared in the config like any other and are not special: a module lists the ones it wants, and `all`
  includes them.
- **A module declares the types it produces**, in its own crate. A consumer depends on that crate. No mission wide
  types crate.
- Access is explicit: a module's config lists what it reads and writes. Shorthands (a group, `all` minus the
  module's own outputs) expand at compile time. A module may not read and write the same topic.
- **One clock read per call.** The executive reads the monotonic clock once, just before each call into a
  module, and uses that time for the module's handles (`writer(now)`) and passes it to `Slot::init`/`step`, which
  give it to the `Ctx`. So a sample's `updated_at` is the time the writer's `ctx.monotonic()` shows.
- **Publishing happens in the writer's step** and is visible at once to modules that step later; a module that
  steps earlier sees it on the next tick. The macro **warns** about a reader that runs before its writer.
- **Two kinds of topic**, state and queued (below), with one underlying `Topic` (storage, `seq`, publish).

## State topics
- One slot per topic: the value, `seq: u32` (up by one per publish) and `updated_at`. Lossy on purpose: two
  publishes in a tick leave the last, and `seq` shows how many.
- **There is no copy per reader.** Every reader looks at the same slot through a shared reference. A reader's own
  state is only its `last_seen_seq`, kept by the reader.
- `latest() -> Option<&Sample<T>>`: `None` means never published; stale is `Some` with an old `updated_at` (FDIR
  compares it with now). The reference cannot outlive the reader's step (the writer needs `&mut` in its own step),
  so a module that wants a value later copies it out.

```rust
pub struct Sample<T> { pub value: T, pub seq: u32, pub updated_at: u64 }
```

## Queued topics (fan-out)
- One writer, N readers, **a queue per reader** (`queue::Queue<T, D>`). Each reader gets its own copy of every
  message. Simple, and the memory is small.
- **Settled 2026-11-24**: all readers of one topic share one depth `D` (`QueuedTopic<T, N, D>`, a `[Queue<T, D>;
  N]`), not a depth per reader. Simpler, no macro needed, matches `Queue<T, N>`'s own const-generic shape. A
  reader that genuinely needs a different depth gets its own topic.
- A queue's **policy** is chosen per queue in the config: refuse (the writer keeps its data, for flow controlled
  links) or drop (and count, reported as an event, for lossy ones). Never a silent drop.
- A full queue at one reader never blocks the others: the message goes to every queue that has room.
  `publish` returns `Result<(), Full>` if any queue refused.
- **Flow control** is a credit count, which on one thread is what a counting semaphore is. No blocking:

```rust
if writer.space() >= PDU_BURST { writer.publish(pdu)?; }   // space() = the smallest free count among its readers
```

- **Fan-in**: each producer has its own queue topic, and the consumer (the link module) lists all of them and
  drains them in its step. Fairness (a drain limit per queue per step) is the consumer's business.

## Access: typed handles, and a dyn view from the same topics
Both views are made from the same `Topic`, so there is one storage and one set of rules, and a module author
only meets the second view in the few generic modules.

**Typed (ordinary and pipeline modules).** The module names two structs, as associated types (generic associated
types, `type In<'a>`, because the handles borrow from the exec). The type is checked at compile time: a wrong type,
or a topic the module was not given, does not build.

```rust
// the sensor crate: what it produces
pub struct Reading { pub volts: f32 }
pub struct SensorOut<'a> { pub reading: bus::StateWriter<'a, Reading> }

impl Module for Sensor {
    type In<'a> = ();
    type Out<'a> = SensorOut<'a>;
    fn step(&mut self, ctx: &mut Ctx<'_, Self>) { ctx.out.reading.publish(Reading { volts: 3.3 }); }
}

// the controller crate, which depends on the sensor crate for the type
pub struct ControllerIn<'a> { pub reading: bus::StateReader<'a, sensor::Reading> }
impl Module for Controller {
    type In<'a> = ControllerIn<'a>;
    type Out<'a> = ();
    fn step(&mut self, ctx: &mut Ctx<'_, Self>) {
        if let Some(s) = ctx.inputs.reading.latest() { /* s.value.volts, s.seq, s.updated_at */ }
    }
}
```

`Ctx` gets two fields, `ctx.inputs` and `ctx.out`. By hand the exec builds each call's handles as struct literals
(`ControllerIn { reading: self.reading.reader() }`); `exec!` only generates those fields and literals from a short
config, and every name in it is a plain Rust path that the compiler checks:

```
topics     { sensor_reading: sensor::Reading, apid 0x100 }
sensor     { out: { reading: sensor_reading } }
controller { in:  { reading: sensor_reading } }
```

**Dyn (telemetry, FDIR, recorder).** A module that picks topics by a table cannot use typed handles: its wired
topics have different types, the table gives it an APID as data, and Rust cannot look up a struct member by a
number. So these modules get an erased collection of what they are wired to: a sorted `(apid, &dyn Topic)` array
that the exec makes (binary search by APID). `Topic` is implemented ONCE, generically, for every topic
(`impl<T: Message> Topic for StateTopic<T>`), so the derive writes nothing for it. Wired at compile time,
filtered by the module's table. Alternatives to `dyn` that were considered: a generated `match` per module
(more generated code) and keeping encoded bytes beside every state value (an encode per publish and a second
copy). APIDs are unique, so a table row needs no module id.
- State side (latest value; a queued topic has none): `seq`, `updated_at`, `latest_bytes` (the encoded value).
- Queue side: `pop_encoded(&mut [u8]) -> Option<usize>` (the next message as bytes). FDIR may use it for events.
- Both: `layout() -> &'static [Field]`, the same layout for every topic of a message type.
- Table rows are `(apid, field index, op, limit, ...)`. A field is read from the encoded bytes with the layout
  (below), so a row works on a state topic's latest bytes and on each message popped from a queue.
- Validated once at table load: the APID is in the module's list, the field index is in the layout, the kinds
  and the operator fit, and a row on a queued topic is not a "latest" rule. A bad row rejects the table and
  names the row, using the field's name.
- `compare(Value, Op, Value) -> Result<bool, CompareError>` never panics: integers exact via `i128`, floats as
  `f64`, a NaN is an error, a bool or text only with its own kind and only for equality.

Considered and not chosen: every module resolving typed ids by APID at startup (`ctx.resolve(Id<T>)`). It is one
access style, but a wiring mistake becomes a boot time error instead of a compile error, and each read goes
through a downcast with a can't-happen `None`. The F Prime design (typed ports in generated wiring, ids for
telemetry and commands, serialized ports for generic consumers) is the same split as above.

## Messages and their layout
Decided 2026-11-23, taking the cFS idea (a table reads a field at an offset and a type) and keeping the checks.
- `#[derive(bus::Message)]` writes two things per message type: `encode` (the wire bytes, one line per field, big
  endian, declaration order, with a `WIRE_LEN` constant) and `LAYOUT`, which is DATA, one entry per field:

```rust
const LAYOUT: &[Field] = &[
    Field { name: "volts", offset: 0, kind: Kind::F32 },
    Field { name: "ok",    offset: 4, kind: Kind::Bool },
];
```

- Offsets are into the WIRE layout, never the in-memory struct (Rust does not fix a struct's layout). `name` is
  the Rust field name (`stringify!`); a nested struct is flattened as `parent.child`. Every topic has a layout,
  queued ones too, because the layout belongs to the message type.
- Reading a field is one function written once by hand, in safe Rust, with no cast and no `unsafe`:
  `read(bytes, field) -> Option<Value>` using `bytes.get(offset..offset + n)` and `from_be_bytes`. A bad range is
  `None`, never a panic. Telemetry packing copies `bytes[offset..offset + len]`.
- Rows name a field by INDEX into the layout. Names are for diagnostics, the ground dictionary and (maybe later)
  tables that name fields, resolved to an index once at load.
- `Kind` is exact (`U16`, `F32`, `Text(8)`), for validation and packing. `Value` is widened for comparing
  (`Int(i128)`, `Float(f64)`, `Bool`, `Text`).
- The cost: the generic module reads the ENCODED latest value, so the topic encodes it (up to 3 kB) when asked.
  A module reading many rows of one topic encodes once per step, not once per row.
- Open: nested structs and arrays (an array of 3 as three leaf fields), text kinds, the validate hook (the table
  service has to call the module with its inputs available).

## Events on the bus
- Events keep going into `Ev`'s log queue as today. The bus gets its own queue.
- `ev.send` ALSO pushes to a second queue, the bus queue, only if its filter allows. The filter is a severity
  floor with **Off as one of the options, and Off is the default**, so it does not fill until switched on.
- The filter is switched by a CORE command. The core has its own commands, which reach it through a command sink
  that the link module feeds. No module holds a handle to the filter.
- `Ev` has its own step, at the top of `core.tick`, next to the log writer: it flushes the bus queue into the
  events topic. Events sent in tick N reach telemetry in tick N+1. The one tick of delay was accepted (the events
  are also in the log, and the system log has them). No flush between module steps, and no borrow conflict: the
  topic is written only in the flush, never during a module step.
- The events topic has one writer, the event service (every module sends through `ctx.ev`, but the topic is the
  service's). Telemetry reads it as a queued topic. Telemetry must not send one event per event it reads.
- Later, optional: the log writer becomes another reader of the topic and `Ev`'s log queue goes away.

## The recorder
A module (last in `order`, base rate, reads `all`, bytes only). Records `(apid, seq, updated_at, length, bytes)`
for every state topic whose `seq` changed, and reports `seq` jumps as an event.

## First slice (DONE 2026-11-23)
State topics only, typed, hand wired in the sample deployment. No derive, no APIDs, no dyn view.
1. `bus::StateTopic<T>`, `StateWriter`, `StateReader` with `latest()`, `Sample` (`core-api/src/bus.rs`).
2. `Module::In<'a>` and `Out<'a>`, `Ctx::inputs` and `Ctx::out`.
3. A loop test with two modules (one publishes, one reads), including the order rule
   (`core/tests/bus_loop.rs`).
4. The sample deployment: `Example` publishes `Progress`, a new module `Watcher` (`modules/watcher`) reads it at
   half the rate and reports what it sees, including how many publishes it missed.

## Queued topics, step 1 (DONE 2026-11-24)
Storage only, no handles yet: `bus::QueuedTopic<T, N, D>`, `Policy { Refuse, Drop }` per reader, `publish`,
`pop`/`len`/`is_empty`/`take_refused` by a plain reader index, `space()`. See `core-api/src/bus.rs`.

Then, in this order: `QueueWriter`/`QueueReader` handles and `Module::In`/`Out` wiring for a queued topic (a loop
test like the state topic's), `Ev`'s bus queue and flush, the `bus::Message` derive with field access, APIDs and
the dyn view, the core commands for the events filter, the recorder, telemetry, FDIR.

## Open
- Fan-in fairness for the link module.
- Field kinds the layout supports (arrays, nested, text) and the exact `Value` type.
- The depth of `Ev`'s bus queue, and the shape of the core command that sets its filter.
