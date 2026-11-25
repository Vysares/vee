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
- A **topic** has a name, a message type and an explicit TID, Topic ID (`u16`). The TID is the identity in
  tables, the recorder and the dictionary. It is NOT a CCSDS APID: a deployment that downlinks CCSDS packets
  chooses its own mapping from TIDs to APIDs (an APID is 11 bits, 0x7FF reserved for idle packets, a constraint
  this bus does not need to carry everywhere).
- **TIDs are unique and are all defined in one place: the deployment** (the `topics { .. }` block of `exec!`),
  never in a module crate (two instances of one module would collide). The core's topics are in the same list.
  A duplicate is a compile error; before `exec!` exists, the hand written sample keeps a `tids` module and a
  `const fn` check. The ground dictionary
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
topics     { sensor_reading: sensor::Reading, tid 0x100 }
sensor     { out: { reading: sensor_reading } }
controller { in:  { reading: sensor_reading } }
```

**Dyn (telemetry, FDIR, recorder).** A module that picks topics by a table cannot use typed handles: its wired
topics have different types, the table gives it a TID as data, and Rust cannot look up a struct member by a
number. So these modules get an erased collection of what they are wired to: a sorted `(tid, &dyn Topic)` array
that the exec makes (binary search by TID). `Topic` is implemented ONCE, generically, for every topic
(`impl<T: Message> Topic for StateTopic<T>`), so the derive writes nothing for it. Wired at compile time,
filtered by the module's table. Alternatives to `dyn` that were considered: a generated `match` per module
(more generated code) and keeping encoded bytes beside every state value (an encode per publish and a second
copy). TIDs are unique, so a table row needs no module id.
- State side (latest value; a queued topic has none): `seq`, `updated_at`, `latest_bytes` (the encoded value).
- Queue side: `pop_encoded(&mut [u8]) -> Option<usize>` (the next message as bytes). FDIR may use it for events.
- Both: `layout() -> &'static [Field]`, the same layout for every topic of a message type.
- Table rows are `(tid, field index, op, limit, ...)`. A field is read from the encoded bytes with the layout
  (below), so a row works on a state topic's latest bytes and on each message popped from a queue.
- Validated once at table load: the TID is in the module's list, the field index is in the layout, the kinds
  and the operator fit, and a row on a queued topic is not a "latest" rule. A bad row rejects the table and
  names the row, using the field's name.
- `compare(Value, Op, Value) -> Result<bool, CompareError>` never panics: integers exact via `i128`, floats as
  `f64`, a NaN is an error, a bool or text only with its own kind and only for equality.

Considered and not chosen: every module resolving typed ids by TID at startup (`ctx.resolve(Id<T>)`). It is one
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
A module (last in `order`, base rate, reads `all`, bytes only). Records `(tid, seq, updated_at, length, bytes)`
for every state topic whose `seq` changed, and reports `seq` jumps as an event.

## First slice (DONE 2026-11-23)
State topics only, typed, hand wired in the sample deployment. No derive, no TIDs, no dyn view.
1. `bus::StateTopic<T>`, `StateWriter`, `StateReader` with `latest()`, `Sample` (`core-api/src/bus.rs`).
2. `Module::In<'a>` and `Out<'a>`, `Ctx::inputs` and `Ctx::out`.
3. A loop test with two modules (one publishes, one reads), including the order rule
   (`core/tests/bus_loop.rs`).
4. The sample deployment: `Example` publishes `Progress`, a new module `Watcher` (`modules/watcher`) reads it at
   half the rate and reports what it sees, including how many publishes it missed.

## Queued topics, step 1 (DONE 2026-11-24)
Storage only, no handles yet: `bus::QueuedTopic<T, N, D>`, `Policy { Refuse, Drop }` per reader, `publish`,
`pop`/`len`/`is_empty`/`take_refused` by a plain reader index, `space()`. See `core-api/src/bus.rs`.

## Queued topics, step 2 (DONE 2026-11-24)
`QueueWriter`/`QueueReader` handles, the queued counterpart of `StateWriter`/`StateReader`. One difference:
a queued reader MUTATES (popping removes the message from its own queue), so unlike `StateReader` it borrows the
whole topic mutably, not immutably; in practice this is no different from the writer and readers never
overlapping, because each handle is made fresh for one module call and dropped when it returns. Checked by hand
(temporary code, deleted): a writer has no `pop`, a reader has no `publish`, a reader held across a later write
does not compile (E0499, not holding two `&mut` borrows of the topic at once), a reader of another message type
does not fit. `QueueWriter` has `publish` and `space()`; `QueueReader` has `pop()` only (no `take_refused`: that
stays on `QueuedTopic` itself, for whichever core-level code owns the topic, the same way `Ev`'s log queue is
`pub` and used directly by its consumer).

## Queued topics, step 3 (DONE 2026-11-24)
`Module::In`/`Out` wiring for a queued topic and a loop test, `core/tests/queued_bus_loop.rs`: a `Producer` and
two `Reader`s (the SAME module type, reused, since a reader's behaviour does not depend on which queue or policy
it was wired to), one topic `QueuedTopic<u32, 2, 2>` with reader 0 `Refuse` and reader 1 `Drop`. Checks fan-out
(both readers get their own copy, in order), a FIFO backlog up to the queue's depth for a reader that falls
behind, and that only the `Refuse` reader's fullness makes the writer see `Err(Full)` (a `Drop` reader's
fullness is silent to the writer, counted on the queue itself, found by `exec.feed.take_refused(1)` with no
module involved).

FOUND while writing it (not a bug in `QueuedTopic`, a gap in the test harness, inherited from `bus_loop.rs`):
neither hand written executive ever called `Slot::report_suppressed`, which is also what starts a module's new
tick of throttle counting. Without it, `EV_MAX_PER_TICK` (default 5) becomes a budget for the WHOLE RUN, not
per tick, so a code sent every tick (this test's `Producer`/`Reader`, unlike `bus_loop.rs`'s, which never
happened to run that many ticks) goes silently missing from the 6th occurrence on. Fixed in
`queued_bus_loop.rs`'s `run_tick` by calling `report_suppressed` for every slot before `core.tick`, matching the
documented loop shape (`docs/design.md`, Executive Requirements). `bus_loop.rs` has the same gap, latent only
because its tests stay at or under 5 occurrences per code; not changed, since it is out of scope here.

## Events on the bus, step 1 (DONE 2026-11-24)
`Ev` gets a second queue, `bus` (depth `EV_BUS_QUEUE_DEPTH`, default 64, separate from `log`'s), and a severity
floor filter, `None` (Off) by default. `send` pushes the same event into `bus` too, only if its severity meets
the floor; a full `bus` queue refuses and counts on its own, same as `log`. `Ev::flush_bus(topic)` pops
everything waiting in `bus` and publishes it to a `QueuedTopic<Event, N, D>`, oldest first; a publish a
`Refuse` reader's full queue turns down is just lost and counted there, same as `log_writer.rs` doesn't retry a
full `log`. `set_filter` is the only way to turn the floor on (no module holds it, per the design above); there
is no core command yet to call it, so a deployment today gets no events on the bus until one is written.
NOT wired into `Core::tick`: nothing sizes `N`/`D` yet (that is the events topic's reader count and depth,
which belongs to whatever reads it — telemetry, later in this roadmap), so `flush_bus` is tested standalone
(`core-api/src/ev.rs`) rather than through a loop test, unlike the state and queued topic slices before it.
Wiring `Core::tick` to call it, and a loop test proving an event reaches a reader, waits until something
exists to read the events topic.

## The `bus::Message` derive, with field access (DONE 2026-11-24)
`#[derive(bus::Message)]` (`core-derive/src/message.rs`; the two `#[proc_macro_derive]` entry points stay in
`core-derive/src/lib.rs` as rustc requires, each one's own logic in its own file, `code.rs`/`message.rs`)
writes, on the message struct itself (inherent, not a trait — a typed caller needs no trait import):
`const WIRE_LEN: usize`, `const LAYOUT: &'static [message::Field]` and `fn encode(&self) -> [u8; WIRE_LEN]`.
Supports `bool` and the fixed-width numbers (`u8`..`u64`, `i8`..`i64`, `f32`, `f64`); a field of any other type
is a compile error naming it. `core-api/src/message.rs` has the type-agnostic half, written once by hand:
`Kind` (those 11 kinds), `Field` and `read(bytes, field) -> Option<Value>` (`Value::{Int(i128), Float(f64),
Bool(bool)}`, widened for comparing). See `core-api/tests/message_derive.rs`.

## TIDs and the dyn view, step 1 (DONE 2026-11-24)
`Tid = u16` (`core-api/src/bus.rs`): a topic's identity, explicitly NOT a CCSDS APID (renamed from "APID"
throughout this doc, 2026-11-24, on request: an APID is a CCSDS downlink concept with its own constraints an
internal topic id does not need to carry).

The derive also now writes `impl message::Message for` the struct (same `WIRE_LEN`/`LAYOUT` values, plus
`encode_into(&self, out: &mut [u8]) -> Option<usize>`, which writes `encode()`'s bytes into a caller's buffer
instead of returning a fixed-size array — a generic `Self::WIRE_LEN`-sized array return is not something a
trait method can do on stable Rust). This is the first thing that needed a `Message` TRAIT, not just the
inherent items: `impl<T: message::Message> Topic for StateTopic<T>` (below) is generic over every message
type, and generic code can only reach a type's items through a trait bound.

`bus::Topic`: the dyn view itself, implemented once for `StateTopic<T>` and once for `QueuedTopic<T, N, D>`.
`layout()`, `latest_bytes(out) -> Option<usize>` (raw bytes, for the recorder) and `pop_encoded(reader, out) ->
Option<usize>` match the original sketch; each topic kind leaves the other side's methods at their trait
default (`None`), since a state topic has no queue and a queued topic has no "latest". A refusal never
costs a message: `pop_encoded` checks that `out` can hold the message BEFORE it takes it out of the queue, so a
buffer one byte too small leaves the message waiting (found by a boundary test, 2026-11-24; it used to pop
first and lose it).

Settled while building this (the user asked, while reviewing the `Message` derive step, what stops `read`
from being called with a field and bytes that do not belong together — nothing did: `read(bytes, field)` is a
safe, bounds-checked, but otherwise fully trusting primitive, like `core-types/src/record.rs`'s `field()`):
`Topic::read_latest(index)` is a DEFAULT method, built on `latest_bytes` and `layout` of THIS SAME topic, so a
caller of the dyn view can never pair a field with someone else's bytes — the only thing it supplies is a
plain index, which a table will validate against `layout().len()` once, at load time (not written yet). This
only closes the gap for the "latest value" side; a queued topic's popped message is still read through the
lower level `message::read(bytes, &topic.layout()[index])` once something (telemetry, FDIR) actually
evaluates table rows against queued topics, because one `pop_encoded` can be followed by several field reads
of the same popped message, which a single bundled call cannot express.

Also added: `bus::find(topics: &[(Tid, &dyn Topic)], tid) -> Option<&dyn Topic>`, a binary search by `Tid`,
written once here so every dyn consumer looks a topic up the same way (the `(tid, &dyn Topic)` array itself is
`exec!`'s job, not written yet). `MESSAGE_MAX_BYTES` (new setting, `core-api/src/message.rs`, default 3072):
the stack buffer `read_latest`'s default encodes a value into before reading a field back out of it (the
"Settled" cap, "a message is Copy plain data, up to about 3 kB", finally has a concrete value).

NOT done this step, left for `exec!`/telemetry: a TID uniqueness check (there is only one real topic in the
sample deployment today, `Example`'s `progress`, so a check now would have nothing to check against) and the
`(tid, &dyn Topic)` array and table themselves. Tested in `core-api/tests/dyn_view.rs` (8 tests: a never
published topic has no latest bytes or field, latest bytes match `encode()`, a too-small buffer is `None` not
a partial write, `read_latest` reads the right field by index and is `None` out of range, a queued topic has
no latest only its queue, `find` locates a topic in a sorted list, a generic function reads either topic kind
through one `&dyn Topic`).

Then, in this order: the core commands for the events filter (which will also be what wires `flush_bus` into
`Core::tick`, once the events topic it writes to is sized), the recorder, telemetry, FDIR.

## Open
- Fan-in fairness for the link module.
- Field kinds the layout supports (arrays, nested, text) and the exact `Value` type.
- The depth of `Ev`'s bus queue, and the shape of the core command that sets its filter.
