//! The command router's shared types: see `docs/design.md`, "Command Routing".
//!
//! A command arrives as `RawCmd`, a neutral, decoded-nothing-yet envelope. A module that brings
//! commands in (a link, an RTS) pushes them into its `Sink`, a staging queue; the core pops them.
//! `route` picks the right target by `RawCmd::target` and hands it the raw command, and `ack`
//! sends the received/rejected event for the result; the target (`CommandTarget`) decodes it with
//! its own `#[derive(CmdSet)]` enum, validates it, and queues the typed result alongside a
//! `CmdMeta` (the little of `RawCmd` worth keeping once it is decoded).
//!
//! A module drains its own queue, inside its own `step`, with `ctx.drain_cmds` (`ctx.rs`); the
//! core does the same with `drain` below, since it holds its `Ev` directly. Either way the
//! author supplies only the per-command match, and the drain sends that command's outcome event
//! itself (`send_outcome`: from the module's own source, or the core's). No module author writes
//! ack/nak text by hand, for either half.
//!
//! This lives in `core-api`, not `core-types`, because an ordinary module uses these from its
//! own crate, and modules depend on `core-api` but not `core-types` (see `design.md`'s dependency
//! table). `CmdEvent` is the ack/nak vocabulary for that reason: it is routing infrastructure
//! every module can reach, not one of the core's own specific events
//! (`vee_core_types::core_event::CoreEvent`).

use crate::config;
use crate::ev;
use crate::event;
use crate::queue;
use core::error;
use core::fmt;

/// The derive of the same name: `#[derive(cmd::CmdSet)]` writes a target's command dispatch, an
/// enum of `Command` structs (see `crate::derive_cmd_set`'s doc comment in `core-derive`).
pub use vee_core_derive::CmdSet;
/// The derive of the same name: `#[derive(cmd::Command)]` writes a command struct's wire
/// encoding, decoding and opcode (see `crate::derive_command`'s doc comment in `core-derive`).
pub use vee_core_derive::Command;

config::setting! {
    /// The most bytes of args in one command. Every command's `Args` is this size, so this sets
    /// how much memory a `RawCmd` and a module's command queue take. Default 32, 1..=4096.
    pub CMD_ARGS_MAX_BYTES: usize = 32, 1..=4096;
}

config::setting! {
    /// How many commands one sink's staging queue holds: what a sink's module can submit in one
    /// tick before the core routes them at the start of the next. Default 8, 1..=65535.
    pub CMD_SINK_QUEUE_DEPTH: usize = 8, 1..=65535;
}

/// A command's encoded fields, up to `CMD_ARGS_MAX_BYTES`, held inside the value: the same inline,
/// nothing-allocated shape as `file::Data`. Unlike `file::Data` this is `Copy`: at this size it is
/// small, like `event::Text`, not large.
///
/// The fields are private so the bytes after `len` are always zero (equality is by content, and a
/// shorter write must not leave stale bytes behind from a reused buffer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Args {
    bytes: [u8; CMD_ARGS_MAX_BYTES],
    len: usize,
}

impl Args {
    /// No bytes.
    pub const EMPTY: Args = Args {
        bytes: [0; CMD_ARGS_MAX_BYTES],
        len: 0,
    };

    /// A copy of `bytes`, or `BadArgs` if there are more than `CMD_ARGS_MAX_BYTES`. There is no
    /// truncating constructor: unlike event text, a command's args are never something to lose
    /// part of and carry on with. `#[derive(Command)]`'s generated `encode` never risks needing
    /// one either, since it checks its `wire_len` against `CMD_ARGS_MAX_BYTES` at compile time
    /// (see `core-derive`) and returns the Result from this constructor like any other caller.
    pub fn new(bytes: &[u8]) -> Result<Args, CmdErr> {
        let mut out = [0u8; CMD_ARGS_MAX_BYTES];
        let room = out.get_mut(..bytes.len()).ok_or(CmdErr::BadArgs)?;
        room.copy_from_slice(bytes);
        Ok(Args {
            bytes: out,
            len: bytes.len(),
        })
    }

    /// The bytes held.
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The `N` bytes at `offset`, as an array ready for `from_be_bytes`, or `None` if they do not fit
/// in `bytes`. Used by `#[derive(Command)]`'s generated `decode` (the same logic as
/// `message::chunk`, kept separate rather than shared: see this module's doc comment on why
/// `Command` and `Message` stay independent).
pub fn chunk<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    bytes.get(offset..end)?.try_into().ok()
}

/// A command, decoded only as far as its envelope: who it is for and which one it is, not yet
/// what it means. `target`/`opcode` are to a command what `source`/`code` are to an `Event`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawCmd {
    /// Which command sink it came from (ground, an RTS, ...). Carried through to the ack/nak
    /// text; nothing validates it yet (no sink registry exists: see `docs/bus.md`'s "Deferred").
    pub sink: u8,
    /// The sender's own id for this command, carried through to the ack/nak text. (The
    /// result-for-`seq` delivery `design.md` describes is not built yet; for now this only makes
    /// the ack/nak identifiable.)
    pub seq: u16,
    /// Which module (or the core, `CORE_TARGET`) this command is for.
    pub target: u8,
    /// Which command this is, unique within its target.
    pub opcode: u16,
    pub args: Args,
}

/// What is left of a `RawCmd` once it is decoded: everything the outcome ack/nak text needs,
/// without having to decode `args` again. A module's command queue holds `(TypedCmd, CmdMeta)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdMeta {
    pub sink: u8,
    pub seq: u16,
    pub target: u8,
    pub opcode: u16,
}

impl From<&RawCmd> for CmdMeta {
    fn from(raw: &RawCmd) -> Self {
        CmdMeta {
            sink: raw.sink,
            seq: raw.seq,
            target: raw.target,
            opcode: raw.opcode,
        }
    }
}

/// Why a command was rejected before it ever reached a target's own logic: at `route` (an unknown
/// target or opcode, args that do not match the command's layout, a queue that is full) or at
/// decode/validate (a value `#[derive(Command)]`'s `decode` or a hand-written `validate` refused).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdErr {
    /// No registered target has this `RawCmd::target`.
    UnknownTarget,
    /// The target's `CmdSet` has no command with this opcode.
    UnknownOpcode,
    /// `args` is shorter than the command's wire layout.
    BadArgs,
    /// Decoded, but a hand-written `validate` refused it. The reason is meant for the ack/nak
    /// text and the ground log, not for a caller to match on.
    Invalid(&'static str),
    /// The target's command queue is full.
    QueueFull,
}

impl fmt::Display for CmdErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CmdErr::UnknownTarget => f.write_str("unknown command target"),
            CmdErr::UnknownOpcode => f.write_str("unknown opcode"),
            // Short on purpose: in the ack text it follows a prefix of up to 39 bytes, and the
            // event text is 64 by default. See `every_reason_fits_the_ack_at_the_widest_values`.
            CmdErr::BadArgs => f.write_str("bad args size"),
            CmdErr::Invalid(reason) => write!(f, "invalid: {reason}"),
            CmdErr::QueueFull => f.write_str("command queue is full"),
        }
    }
}

impl error::Error for CmdErr {}

/// What handling a queued command came to, once a target has actually run it. `design.md`:
/// "Success/Fail/Success(pending)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdResult {
    Success,
    /// The reason, for the ack/nak text.
    Fail(&'static str),
    /// Accepted and under way; its real outcome is not reported yet (no result-for-`seq`
    /// mechanism exists to report it later: see this module's doc comment).
    Pending,
}

/// The ack/nak vocabulary every command goes through, whichever target it was for: see this
/// module's doc comment for why this is here and not one of `core_event::CoreEvent`'s variants.
///
/// `event::Code` is written by hand, not with `#[derive(event::Code)]`: that derive's generated
/// code names its own crate as `::vee_core_api`, which only resolves for a consumer outside this
/// crate (`core-types`'s `CoreEvent` is the usual case). The same note is in `ev.rs`'s and
/// `throttle.rs`'s own hand-written test codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdEvent {
    /// `route` found the target and it queued the command.
    Accepted,
    /// `route` could not queue the command: the reason is in the text (see `CmdErr`'s `Display`).
    Rejected,
    /// A target's `drain` ran the command and it succeeded, or it is `Pending`.
    Executed,
    /// A target's `drain` ran the command and it failed.
    Failed,
}

impl event::Code for CmdEvent {
    fn code(&self) -> u16 {
        *self as u16
    }

    fn severity(&self) -> event::Severity {
        match self {
            CmdEvent::Accepted | CmdEvent::Executed => event::Severity::Info,
            CmdEvent::Rejected | CmdEvent::Failed => event::Severity::Err,
        }
    }

    fn index(&self) -> usize {
        *self as usize
    }

    fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(CmdEvent::Accepted),
            1 => Some(CmdEvent::Rejected),
            2 => Some(CmdEvent::Executed),
            3 => Some(CmdEvent::Failed),
            _ => None,
        }
    }
}

/// The core's own target id, for routing and for the `source` the ack/nak events above are sent
/// from: 0, matching `vee_core_types::core_event::SOURCE`. Defined again here, not borrowed from
/// `core-types`, because a module's own call to `drain` must not need that crate (modules depend
/// on `core-api`, not `core-types`). `core-types` has a test that the two values agree.
pub const CORE_TARGET: u8 = 0;

/// Something `route` can hand a decoded-enough command to: a module's own command queue (or the
/// core's). `accept` is everything `design.md` means by "the router decodes and validates each
/// command once": decode `raw.args` with this target's own `CmdSet`, run its `validate`, and queue
/// the typed result with a `CmdMeta`. Nothing here runs the command or sends an event.
pub trait CommandTarget {
    fn accept(&mut self, raw: &RawCmd) -> Result<(), CmdErr>;
}

/// Finds the entry in `targets` whose id is `raw.target` and hands it the command, or
/// `UnknownTarget` if there is none. `targets` is small (at most one entry per module), so a
/// linear scan is simplest; nothing requires it to be sorted by id, unlike `bus::find`'s table.
pub fn route(targets: &mut [(u8, &mut dyn CommandTarget)], raw: RawCmd) -> Result<(), CmdErr> {
    match targets.iter_mut().find(|(id, _)| *id == raw.target) {
        Some((_, target)) => target.accept(&raw),
        None => Err(CmdErr::UnknownTarget),
    }
}

/// Sends the received ack (`CmdEvent::Accepted`) if `result` is `Ok`, or the rejected nak
/// (`CmdEvent::Rejected`, with the `CmdErr` in the text) if it is not. `result` is what `route`
/// (or a target's `accept`) returned for `raw`. Every command goes through this once, whichever
/// way it was routed, so the text is written in one place.
pub fn ack(ev: &mut ev::Ev, raw: &RawCmd, result: &Result<(), CmdErr>, now: u64) {
    match result {
        Ok(()) => ev.send(
            CORE_TARGET,
            now,
            CmdEvent::Accepted,
            format_args!(
                "cmd {}/{} t{} op{:#06x}: accepted",
                raw.sink, raw.seq, raw.target, raw.opcode
            ),
        ),
        Err(err) => ev.send(
            CORE_TARGET,
            now,
            CmdEvent::Rejected,
            format_args!(
                "cmd {}/{} t{} op{:#06x}: rejected: {err}",
                raw.sink, raw.seq, raw.target, raw.opcode
            ),
        ),
    }
}

/// What a module uses to submit commands: a sink with only `submit` on it. A module that submits
/// commands names this in its `Out` (`type Out<'a> = ...`), and the executive makes one for each
/// call from the sink it owns, as it makes a topic's writer. With no `pop`, a module cannot take
/// commands out of a sink, only put its own in (the same idea as `ctx::EvHandle`).
pub struct SinkHandle<'a> {
    sink: &'a mut Sink,
}

impl SinkHandle<'_> {
    /// Queues a command from this sink: `Sink::submit`, which stamps the sink's id on it and
    /// gives `QueueFull` if the sink is full.
    pub fn submit(&mut self, seq: u16, target: u8, opcode: u16, args: Args) -> Result<(), CmdErr> {
        self.sink.submit(seq, target, opcode, args)
    }
}

/// A command sink: the staging queue a module pushes its commands into, and the core drains
/// (`design.md`, "Command Routing"). One per sink, owned by the executive, which lends a module
/// only the way to `submit` (through its `Ctx`); `pop` is the core's.
pub struct Sink {
    /// The sink's id from the deployment. It is stamped on every command, so a module cannot
    /// send as another source.
    id: u8,
    queue: queue::Queue<RawCmd, CMD_SINK_QUEUE_DEPTH>,
}

impl Sink {
    pub const fn new(id: u8) -> Self {
        Sink {
            id,
            queue: queue::Queue::new(),
        }
    }

    /// Queues a command from this sink, or `QueueFull` if the queue is full: the submitter knows
    /// at once, and the command is not queued.
    pub fn submit(&mut self, seq: u16, target: u8, opcode: u16, args: Args) -> Result<(), CmdErr> {
        let raw = RawCmd {
            sink: self.id,
            seq,
            target,
            opcode,
            args,
        };
        self.queue.push(raw).map_err(|_| CmdErr::QueueFull)
    }

    /// The handle a module submits through, for one call.
    pub fn handle(&mut self) -> SinkHandle<'_> {
        SinkHandle { sink: self }
    }

    /// The oldest queued command. The core's router calls this.
    pub fn pop(&mut self) -> Option<RawCmd> {
        self.queue.pop()
    }
}

/// Pops every command waiting in `queue`, in order, calling `handle` on each and then sending its
/// outcome event. A module's `step` is the only caller: this is how it drains its own queue.
///
/// `handle` takes `ev` as a plain argument, not by capturing it, so a `handle` that itself needs
/// to touch `ev` (as `Ev::set_filter` does) does not hold a borrow of it for the whole call: each
/// iteration re-borrows `ev` for `handle`, then again afterwards to send the outcome event.
pub fn drain<C, const N: usize>(
    queue: &mut queue::Queue<(C, CmdMeta), N>,
    ev: &mut ev::Ev,
    now: u64,
    mut handle: impl FnMut(C, &mut ev::Ev) -> CmdResult,
) {
    while let Some((cmd, meta)) = queue.pop() {
        let result = handle(cmd, ev);
        send_outcome(ev, CORE_TARGET, now, meta, result);
    }
}

/// The outcome half of the ack/nak text, shared by every target's drain: the core's `drain` above
/// (source `CORE_TARGET`) and `Ctx::drain_cmds` (the module's own source). `source` and `now` say
/// who ran the command and when; the text has the command's own sink, seq, target and opcode.
/// It goes straight to `ev`, not through a throttle: every command gets its outcome.
pub(crate) fn send_outcome(
    ev: &mut ev::Ev,
    source: u8,
    now: u64,
    meta: CmdMeta,
    result: CmdResult,
) {
    match result {
        CmdResult::Success => ev.send(
            source,
            now,
            CmdEvent::Executed,
            format_args!(
                "cmd {}/{} t{} op{:#06x}: succeeded",
                meta.sink, meta.seq, meta.target, meta.opcode
            ),
        ),
        CmdResult::Fail(reason) => ev.send(
            source,
            now,
            CmdEvent::Failed,
            format_args!(
                "cmd {}/{} t{} op{:#06x}: failed: {reason}",
                meta.sink, meta.seq, meta.target, meta.opcode
            ),
        ),
        CmdResult::Pending => ev.send(
            source,
            now,
            CmdEvent::Executed,
            format_args!(
                "cmd {}/{} t{} op{:#06x}: pending",
                meta.sink, meta.seq, meta.target, meta.opcode
            ),
        ),
    }
}

/// The popping end of a command queue, with the queue's depth `N` left out, so a `Ctx` can hold
/// any module's queue whatever its depth. Only `Inbox` uses it.
trait Pop<C> {
    fn pop(&mut self) -> Option<(C, CmdMeta)>;
}

impl<C, const N: usize> Pop<C> for queue::Queue<(C, CmdMeta), N> {
    fn pop(&mut self) -> Option<(C, CmdMeta)> {
        queue::Queue::pop(self)
    }
}

/// A module's command queue, as its `Ctx` holds it (`Module::Cmd` is `C`). The executive makes
/// one for each call from the queue it owns, as it makes a topic's reader; a module cannot pop
/// from it itself: `Ctx::drain_cmds` does, and sends each outcome.
pub struct Inbox<'a, C> {
    queue: Option<&'a mut dyn Pop<C>>,
}

impl<'a, C> Inbox<'a, C> {
    /// An inbox on `queue`, which is what a target's `accept` fills.
    pub fn new<const N: usize>(queue: &'a mut queue::Queue<(C, CmdMeta), N>) -> Self {
        Inbox { queue: Some(queue) }
    }

    /// An inbox that is always empty: for a module with no commands (`type Cmd = ();`).
    pub const fn empty() -> Self {
        Inbox { queue: None }
    }

    /// The oldest queued command, if any.
    pub(crate) fn pop(&mut self) -> Option<(C, CmdMeta)> {
        self.queue.as_mut()?.pop()
    }
}

#[cfg(test)]
mod tests {
    use super as cmd;
    use crate::ev;
    use crate::event;
    use crate::event::Code as _;
    use crate::queue;

    /// A single event code for the borrow-shape test below, written by hand (the derive's output
    /// does not resolve inside this crate: see the same note in `ev.rs`'s own tests).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestCode;

    impl event::Code for TestCode {
        fn code(&self) -> u16 {
            0
        }
        fn severity(&self) -> event::Severity {
            event::Severity::Warn
        }
        fn index(&self) -> usize {
            0
        }
        fn from_index(index: usize) -> Option<Self> {
            (index == 0).then_some(TestCode)
        }
    }

    /// `route`, then `ack`: what the core does for every command.
    fn route_and_ack(
        targets: &mut [(u8, &mut dyn cmd::CommandTarget)],
        raw: cmd::RawCmd,
        ev: &mut ev::Ev,
        now: u64,
    ) -> Result<(), cmd::CmdErr> {
        let result = cmd::route(targets, raw);
        cmd::ack(ev, &raw, &result, now);
        result
    }

    fn raw(target: u8, opcode: u16, bytes: &[u8]) -> cmd::RawCmd {
        cmd::RawCmd {
            sink: 1,
            seq: 42,
            target,
            opcode,
            args: cmd::Args::new(bytes).unwrap(),
        }
    }

    #[test]
    fn args_roundtrip_within_capacity() {
        let args = cmd::Args::new(&[1, 2, 3]).unwrap();
        assert_eq!(args.as_slice(), &[1, 2, 3]);
        assert_eq!(args.len(), 3);
        assert!(!args.is_empty());
    }

    #[test]
    fn args_new_refuses_more_than_the_limit() {
        let over = [7u8; cmd::CMD_ARGS_MAX_BYTES + 1];
        assert_eq!(cmd::Args::new(&over), Err(cmd::CmdErr::BadArgs));
    }

    #[test]
    fn empty_args_has_no_bytes() {
        assert!(cmd::Args::EMPTY.is_empty());
        assert_eq!(cmd::Args::EMPTY.as_slice(), &[]);
    }

    #[test]
    fn chunk_reads_the_bytes_at_an_offset() {
        let bytes = [1, 2, 3, 4];
        assert_eq!(cmd::chunk::<2>(&bytes, 1), Some([2, 3]));
    }

    #[test]
    fn chunk_is_none_when_it_would_run_past_the_end() {
        let bytes = [1, 2, 3];
        assert_eq!(cmd::chunk::<4>(&bytes, 0), None);
        assert_eq!(cmd::chunk::<1>(&bytes, 3), None);
    }

    #[test]
    fn chunk_does_not_overflow_at_the_end_of_usize() {
        assert_eq!(cmd::chunk::<1>(&[1, 2, 3], usize::MAX), None);
    }

    #[test]
    fn cmd_meta_is_raw_cmd_without_its_args() {
        let raw = raw(3, 0x10, &[9]);
        assert_eq!(
            cmd::CmdMeta::from(&raw),
            cmd::CmdMeta {
                sink: 1,
                seq: 42,
                target: 3,
                opcode: 0x10,
            }
        );
    }

    /// A `CommandTarget` double: queues whatever it accepts, or refuses if told to be busy.
    struct Fake {
        accepted: queue::Queue<cmd::RawCmd, 4>,
        busy: bool,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                accepted: queue::Queue::new(),
                busy: false,
            }
        }
    }

    impl cmd::CommandTarget for Fake {
        fn accept(&mut self, raw: &cmd::RawCmd) -> Result<(), cmd::CmdErr> {
            if self.busy {
                return Err(cmd::CmdErr::QueueFull);
            }
            self.accepted.push(*raw).map_err(|_| cmd::CmdErr::QueueFull)
        }
    }

    #[test]
    fn route_hands_the_command_to_the_target_with_the_matching_id() {
        let mut a = Fake::new();
        let mut b = Fake::new();
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 2] = [(1, &mut a), (2, &mut b)];

        cmd::route(&mut targets, raw(2, 0x01, &[])).unwrap();

        assert_eq!(a.accepted.len(), 0);
        assert_eq!(b.accepted.len(), 1);
    }

    #[test]
    fn route_with_no_matching_target_is_unknown_target() {
        let mut a = Fake::new();
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 1] = [(1, &mut a)];

        assert_eq!(
            cmd::route(&mut targets, raw(9, 0x01, &[])),
            Err(cmd::CmdErr::UnknownTarget)
        );
    }

    #[test]
    fn route_passes_on_the_targets_own_refusal() {
        let mut a = Fake::new();
        a.busy = true;
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 1] = [(1, &mut a)];

        assert_eq!(
            cmd::route(&mut targets, raw(1, 0x01, &[])),
            Err(cmd::CmdErr::QueueFull)
        );
    }

    #[test]
    fn route_and_ack_sends_accepted_on_success() {
        let mut a = Fake::new();
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 1] = [(1, &mut a)];
        let mut sent = ev::Ev::new();

        route_and_ack(&mut targets, raw(1, 0x05, &[]), &mut sent, 10).unwrap();

        let event = sent.log.pop().unwrap();
        assert_eq!(event.source, cmd::CORE_TARGET);
        assert_eq!(event.code, cmd::CmdEvent::Accepted.code());
        assert!(event.text.as_str().contains("accepted"));
    }

    #[test]
    fn route_and_ack_sends_rejected_with_the_reason_on_failure() {
        let mut a = Fake::new();
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 1] = [(1, &mut a)];
        let mut sent = ev::Ev::new();

        let result = route_and_ack(&mut targets, raw(9, 0x05, &[]), &mut sent, 10);
        assert_eq!(result, Err(cmd::CmdErr::UnknownTarget));

        let event = sent.log.pop().unwrap();
        assert_eq!(event.code, cmd::CmdEvent::Rejected.code());
        assert!(event.text.as_str().contains("unknown command target"));
    }

    #[test]
    fn ack_text_is_not_cut_off_even_at_the_widest_field_values() {
        // sink and target are the widest a u8 gets, seq and opcode the widest a u16 gets, so this
        // is the longest the "cmd s/seq tT opOP: rejected: ..." prefix can ever be. If the format
        // ever grows past `event::EV_TEXT_MAX_BYTES` (64 by default), this starts failing instead
        // of silently losing the end of real ack text on the ground.
        let mut a = Fake::new();
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 1] = [(1, &mut a)];
        let mut sent = ev::Ev::new();
        let widest = cmd::RawCmd {
            sink: u8::MAX,
            seq: u16::MAX,
            target: u8::MAX,
            opcode: u16::MAX,
            args: cmd::Args::EMPTY,
        };

        route_and_ack(&mut targets, widest, &mut sent, 0).unwrap_err();

        let event = sent.log.pop().unwrap();
        assert_eq!(
            event.text.as_str(),
            "cmd 255/65535 t255 op0xffff: rejected: unknown command target"
        );
    }

    #[test]
    fn a_sink_stamps_its_own_id_on_what_is_submitted() {
        let mut sink = cmd::Sink::new(7);
        sink.submit(3, 2, 0x10, cmd::Args::new(&[9]).unwrap())
            .unwrap();
        let popped = sink.pop().unwrap();
        assert_eq!(popped.sink, 7);
        assert_eq!(popped.seq, 3);
        assert_eq!(popped.target, 2);
        assert_eq!(popped.opcode, 0x10);
        assert_eq!(popped.args.as_slice(), &[9]);
        assert_eq!(sink.pop(), None);
    }

    #[test]
    fn a_sink_hands_commands_back_oldest_first() {
        let mut sink = cmd::Sink::new(1);
        sink.submit(1, 0, 1, cmd::Args::EMPTY).unwrap();
        sink.submit(2, 0, 1, cmd::Args::EMPTY).unwrap();
        assert_eq!(sink.pop().unwrap().seq, 1);
        assert_eq!(sink.pop().unwrap().seq, 2);
    }

    #[test]
    fn a_full_sink_refuses_and_keeps_what_it_has() {
        let mut sink = cmd::Sink::new(1);
        for _ in 0..cmd::CMD_SINK_QUEUE_DEPTH {
            sink.submit(1, 0, 1, cmd::Args::EMPTY).unwrap();
        }
        assert_eq!(
            sink.submit(999, 0, 1, cmd::Args::EMPTY),
            Err(cmd::CmdErr::QueueFull)
        );
        // The refused command (seq 999) was not queued: what comes out is the earlier ones.
        assert_eq!(sink.pop().unwrap().seq, 1);
    }

    #[test]
    fn a_handle_submits_into_its_sink_with_the_sinks_id() {
        let mut sink = cmd::Sink::new(4);
        sink.handle()
            .submit(1, 2, 0x30, cmd::Args::new(&[5, 6]).unwrap())
            .unwrap();
        let popped = sink.pop().unwrap();
        assert_eq!(popped.sink, 4);
        assert_eq!(popped.args.as_slice(), &[5, 6]);
    }

    #[test]
    fn handles_made_one_after_another_share_the_same_queue() {
        let mut sink = cmd::Sink::new(1);
        sink.handle().submit(1, 0, 1, cmd::Args::EMPTY).unwrap();
        sink.handle().submit(2, 0, 1, cmd::Args::EMPTY).unwrap();
        assert_eq!(sink.pop().unwrap().seq, 1);
        assert_eq!(sink.pop().unwrap().seq, 2);
    }

    #[test]
    fn a_handle_reports_a_full_sink_to_its_caller() {
        let mut sink = cmd::Sink::new(1);
        for _ in 0..cmd::CMD_SINK_QUEUE_DEPTH {
            sink.handle().submit(1, 0, 1, cmd::Args::EMPTY).unwrap();
        }
        assert_eq!(
            sink.handle().submit(2, 0, 1, cmd::Args::EMPTY),
            Err(cmd::CmdErr::QueueFull)
        );
    }

    #[test]
    fn a_sink_that_was_emptied_takes_commands_again() {
        let mut sink = cmd::Sink::new(1);
        for _ in 0..cmd::CMD_SINK_QUEUE_DEPTH {
            sink.submit(1, 0, 1, cmd::Args::EMPTY).unwrap();
        }
        sink.pop().unwrap();
        assert_eq!(sink.submit(2, 0, 1, cmd::Args::EMPTY), Ok(()));
    }

    #[test]
    fn each_ack_event_has_its_own_code_severity_and_index_and_looks_up_by_index() {
        use event::Code as _;
        let events = [
            (cmd::CmdEvent::Accepted, 0, event::Severity::Info),
            (cmd::CmdEvent::Rejected, 1, event::Severity::Err),
            (cmd::CmdEvent::Executed, 2, event::Severity::Info),
            (cmd::CmdEvent::Failed, 3, event::Severity::Err),
        ];
        for (event, number, severity) in events {
            assert_eq!(event.code(), number);
            assert_eq!(event.index(), usize::from(number));
            assert_eq!(event.severity(), severity);
            assert_eq!(cmd::CmdEvent::from_index(event.index()), Some(event));
        }
        // One past the last event is no event, and neither is the largest index.
        assert_eq!(cmd::CmdEvent::from_index(4), None);
        assert_eq!(cmd::CmdEvent::from_index(usize::MAX), None);
    }

    #[test]
    fn every_reason_fits_the_ack_at_the_widest_values() {
        // Every reason this crate writes, after the longest prefix an ack can have. (`Invalid`'s
        // reason is the target's own text, so it can be as long as its author likes: it is cut
        // off at the end like any event text, which is why it is not here.)
        let widest = cmd::RawCmd {
            sink: u8::MAX,
            seq: u16::MAX,
            target: u8::MAX,
            opcode: u16::MAX,
            args: cmd::Args::EMPTY,
        };
        for err in [
            cmd::CmdErr::UnknownTarget,
            cmd::CmdErr::UnknownOpcode,
            cmd::CmdErr::BadArgs,
            cmd::CmdErr::QueueFull,
        ] {
            let mut sent = ev::Ev::new();
            cmd::ack(&mut sent, &widest, &Err(err), 0);
            let text = sent.log.pop().unwrap().text;
            assert!(!text.as_str().ends_with('~'), "cut off: {}", text.as_str());
        }
    }

    #[test]
    fn drain_runs_every_queued_command_in_order_and_sends_its_outcome() {
        let mut queue = queue::Queue::<(u8, cmd::CmdMeta), 4>::new();
        let meta = cmd::CmdMeta {
            sink: 1,
            seq: 1,
            target: 1,
            opcode: 1,
        };
        queue.push((1, meta)).unwrap();
        queue.push((2, meta)).unwrap();
        let mut sent = ev::Ev::new();

        let mut seen = [0u8; 2];
        let mut next = 0;
        cmd::drain(&mut queue, &mut sent, 5, |value, _ev| {
            seen[next] = value;
            next += 1;
            cmd::CmdResult::Success
        });

        assert_eq!(seen, [1, 2]);
        assert_eq!(sent.log.pop().unwrap().code, cmd::CmdEvent::Executed.code());
        assert_eq!(sent.log.pop().unwrap().code, cmd::CmdEvent::Executed.code());
        assert!(queue.is_empty());
    }

    #[test]
    fn drains_handle_can_mutate_ev_itself_without_a_borrow_conflict() {
        // The reason `handle` takes `ev` as a plain argument: a real handler (such as the core's
        // own `SetEventFilter`) mutates `ev` directly, and `drain` still needs `ev` afterwards to
        // send the outcome event. This proves that actually compiles and works, not just reads
        // right on paper.
        let mut queue = queue::Queue::<(u8, cmd::CmdMeta), 1>::new();
        let meta = cmd::CmdMeta {
            sink: 1,
            seq: 1,
            target: 1,
            opcode: 1,
        };
        queue.push((0, meta)).unwrap();
        let mut sent = ev::Ev::new();

        cmd::drain(&mut queue, &mut sent, 5, |_value, ev| {
            ev.set_filter(Some(event::Severity::Warn));
            cmd::CmdResult::Success
        });

        // The handler's own mutation stuck, and the outcome event after it was still sent.
        sent.send(1, 5, TestCode, format_args!("after"));
        assert_eq!(sent.bus.len(), 1); // the filter the handler set let it through
        assert_eq!(sent.log.pop().unwrap().code, cmd::CmdEvent::Executed.code());
    }

    #[test]
    fn drain_reports_fail_and_pending_outcomes_with_their_own_text() {
        let mut queue = queue::Queue::<(u8, cmd::CmdMeta), 2>::new();
        let meta = cmd::CmdMeta {
            sink: 1,
            seq: 1,
            target: 1,
            opcode: 1,
        };
        queue.push((0, meta)).unwrap();
        queue.push((1, meta)).unwrap();
        let mut sent = ev::Ev::new();

        cmd::drain(&mut queue, &mut sent, 5, |value, _ev| match value {
            0 => cmd::CmdResult::Fail("no"),
            _ => cmd::CmdResult::Pending,
        });

        let first = sent.log.pop().unwrap();
        assert_eq!(first.code, cmd::CmdEvent::Failed.code());
        assert!(first.text.as_str().contains("no"));
        let second = sent.log.pop().unwrap();
        assert_eq!(second.code, cmd::CmdEvent::Executed.code());
        assert!(second.text.as_str().contains("pending"));
    }
}
