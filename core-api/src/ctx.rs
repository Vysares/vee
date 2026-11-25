//! `Ctx`: everything a module can reach, and the only way it reaches it.
//!
//! The executive builds a `Ctx` for each call into a module (`init` or `step`), and the module
//! uses it for that call only. It is made of pieces the core owns, which the module borrows
//! through the `Ctx`: a module never holds a reference to core state of its own. Right now it has
//! the event service, the time and the reset info. The bus, files, tables and devices will be
//! added as they are written, and later the executive's config will decide which of them each
//! module's `Ctx` gets.
//!
//! `Ctx` is generic over the module `M`: what is inside comes from what `M` declares: its event
//! enum (`M::Event`) and the bus handles it reads and writes (`M::In` and `M::Out`).

use crate::cmd;
use crate::ev;
use crate::event;
use crate::module;
use crate::reset;
use crate::throttle;
use core::fmt;
use core::marker::PhantomData;

/// What a module is given for one call of `init` or `step`.
pub struct Ctx<'a, M: module::Module> {
    /// Sends events. See `EvHandle`.
    pub ev: EvHandle<'a, M::Event>,
    /// How the previous run ended, and how many runs have been unclean in a row. The same for
    /// every module, and it does not change during a run.
    pub reset: reset::ResetInfo,
    /// The bus topics the module reads: what it declared as `M::In`.
    pub inputs: M::In<'a>,
    /// The bus topics the module writes: what it declared as `M::Out`.
    pub out: M::Out<'a>,
    /// The module's command queue: only `drain_cmds` reads it.
    cmds: cmd::Inbox<'a, M::Cmd>,
    monotonic_us: u64,
}

impl<'a, M: module::Module> Ctx<'a, M> {
    /// Only the executive makes a `Ctx`. It borrows the pieces it owns:
    /// - `ev` and `throttle`: the one event queue, and this module's throttle.
    /// - `source`: the module's id, which goes into every event it sends.
    /// - `monotonic_us`: the time, read from the clock once, before this call. It stays the same
    ///   for the whole call.
    /// - `reset`: from the reset log, read at startup.
    /// - `inputs` and `out`: the module's bus handles, made by the executive from the topics it
    ///   owns. They have the same lifetime `'a` as the other borrows, because all of them last
    ///   for this one call.
    /// - `cmds`: the module's command queue (`Inbox::empty()` for a module with no commands).
    // TODO(revisit): too many arguments (8 of clippy's 7), allowed for now. Every new thing a
    // module can reach (tables, devices) adds another one, to this and to every caller. The plan:
    // group the per call handles `inputs`, `out` and `cmds` into one struct, so the list shrinks and
    // each is named at the call site. Remove this `allow` when that is done. See progress.md, item 95.
    #[allow(
        clippy::too_many_arguments,
        reason = "known and temporary: the per call handles will be grouped into one struct (TODO above)"
    )]
    pub fn new(
        ev: &'a mut ev::Ev,
        throttle: &'a mut throttle::Throttle,
        source: u8,
        monotonic_us: u64,
        reset: reset::ResetInfo,
        inputs: M::In<'a>,
        out: M::Out<'a>,
        cmds: cmd::Inbox<'a, M::Cmd>,
    ) -> Self {
        Ctx {
            ev: EvHandle {
                ev,
                throttle,
                source,
                monotonic_us,
                code: PhantomData,
            },
            reset,
            inputs,
            out,
            cmds,
            monotonic_us,
        }
    }

    /// Handles every command waiting in the module's queue, oldest first. Call it from `step`.
    ///
    /// `handle` is the module's part: it matches on the command (an `M::Cmd`), does the work, and
    /// returns what came of it (`cmd::CmdResult`). `drain_cmds` does the rest: it sends the
    /// command's outcome event ("succeeded", "failed: reason" or "pending") from this module's
    /// source, so a module never writes ack text. The outcome is not throttled: every command
    /// gets one.
    ///
    /// `handle` gets `ctx` as its second argument, so it can send events or write outputs. It
    /// cannot use the `ctx` it was called on instead, because `drain_cmds` has that borrowed for
    /// the whole call.
    pub fn drain_cmds(&mut self, mut handle: impl FnMut(M::Cmd, &mut Self) -> cmd::CmdResult) {
        while let Some((command, meta)) = self.cmds.pop() {
            let result = handle(command, self);
            cmd::send_outcome(
                &mut *self.ev.ev,
                self.ev.source,
                self.monotonic_us,
                meta,
                result,
            );
        }
    }

    /// Microseconds since this boot. It restarts at every boot and never jumps. It is the time
    /// when the executive made this `Ctx`, so it does not change during one call.
    pub fn monotonic(&self) -> u64 {
        self.monotonic_us
    }
}

/// What a module uses to send events (`ctx.ev`). It has only `send`, so a module cannot read
/// events, or send one as another source.
///
/// `E` is the module's own event enum. `send` takes only an `E`, so the compiler stops a module
/// from sending another module's events.
pub struct EvHandle<'a, E: event::Code> {
    ev: &'a mut ev::Ev,
    throttle: &'a mut throttle::Throttle,
    source: u8,
    monotonic_us: u64,
    /// `E` is only used by `send`, not stored. This ties the handle to one enum of events.
    code: PhantomData<E>,
}

impl<E: event::Code> EvHandle<'_, E> {
    /// Sends an event with this text. The event's severity comes from `code`, and its source and
    /// time are the module's and this call's.
    ///
    /// It never fails, because the module can do nothing about a failure (see `Ev::send`). If
    /// this event has already been sent the most it may be this tick (`EV_MAX_PER_TICK`), it is
    /// not sent at all: it is counted, and the core reports the count as an event at the start
    /// of the next tick.
    pub fn send(&mut self, code: E, text: fmt::Arguments<'_>) {
        if self.throttle.throttle(&code).is_some() {
            return;
        }
        self.ev.send(self.source, self.monotonic_us, code, text);
    }
}
