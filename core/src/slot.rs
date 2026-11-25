//! `Slot<M>`: everything the executive keeps for one module, and the three things it does with it
//! (`init`, `report_suppressed`, `step`). It is the same for every module, so it is written
//! once, here, and the deployment's executive only holds one `Slot` for each module and calls
//! them in order. (That is all `exec!` will have to generate.)
//!
//! A module never sees any of this: it sees the `Ctx` that a `Slot` builds for each call.

use crate::suppressed;
use vee_core_api::cmd;
use vee_core_api::ctx;
use vee_core_api::ev;
use vee_core_api::module;
use vee_core_api::reset;
use vee_core_api::throttle;
use vee_core_types::core_event;

pub struct Slot<M: module::Module> {
    module: M,
    throttle: throttle::Throttle,
    /// The module's id. It goes into every event the module sends.
    source: u8,
    /// The module steps on every `period`th tick. 1 is every tick.
    period: u64,
}

impl<M: module::Module> Slot<M> {
    /// `source` must be different for every module, and not 0, which is the core's.
    pub fn new(module: M, source: u8, period: u64) -> Self {
        Slot {
            module,
            throttle: throttle::Throttle::new(),
            source,
            period,
        }
    }

    /// Startup: builds the module's `Ctx` and calls `init`. `time` is the monotonic time, which
    /// the executive read once just before this call: it is the time the module sees in
    /// `ctx.monotonic()`, and the executive uses the same time for the bus handles it makes
    /// (`StateTopic::writer(time)`), so a sample's `updated_at` and the module's clock agree.
    /// `inputs` and `out` are the module's bus handles, which the executive makes from the topics
    /// it owns. `cmds` is the module's command queue (`cmd::Inbox::new(&mut queue)`, or
    /// `cmd::Inbox::empty()` for a module with no commands).
    ///
    /// All the borrows share the one lifetime `'a`, the length of this call. (`M::In<'a>` and
    /// `M::Out<'a>` cannot be shortened by the compiler, because it does not know how a module's
    /// own type uses `'a`, so the other borrows are made to match them instead.)
    pub fn init<'a>(
        &'a mut self,
        ev: &'a mut ev::Ev,
        time: u64,
        reset: reset::ResetInfo,
        inputs: M::In<'a>,
        out: M::Out<'a>,
        cmds: cmd::Inbox<'a, M::Cmd>,
    ) {
        log::debug!("source {} init at {time} us", self.source);
        let mut ctx = ctx::Ctx::<M>::new(
            ev,
            &mut self.throttle,
            self.source,
            time,
            reset,
            inputs,
            out,
            cmds,
        );
        self.module.init(&mut ctx);
    }

    /// Top of a tick: sends the core's report of what this module had suppressed in the tick
    /// before, as events from the core at `now`. It also starts the module's new tick of counting.
    pub fn report_suppressed(&mut self, ev: &mut ev::Ev, now: u64) {
        suppressed::report::<M::Event, _>(&mut self.throttle, self.source, |code, text| {
            ev.send(core_event::SOURCE, now, code, text);
        });
    }

    /// Calls `step` if this tick is one of the module's. `time` is the monotonic time the
    /// executive read just before this call (see `init`).
    // TODO(revisit): too many arguments (8 of clippy's 7), allowed for now. Every new thing a
    // module can reach (tables, devices) adds another one, to this, to `Ctx::new` and to every caller. The plan:
    // group the per call handles `inputs`, `out` and `cmds` into one struct, so the list shrinks and
    // each is named at the call site. Remove this `allow` when that is done. See progress.md, item 95.
    #[allow(
        clippy::too_many_arguments,
        reason = "known and temporary: the per call handles will be grouped into one struct (TODO above)"
    )]
    pub fn step<'a>(
        &'a mut self,
        tick: u64,
        ev: &'a mut ev::Ev,
        time: u64,
        reset: reset::ResetInfo,
        inputs: M::In<'a>,
        out: M::Out<'a>,
        cmds: cmd::Inbox<'a, M::Cmd>,
    ) {
        if tick.checked_rem(self.period) != Some(0) {
            return;
        }
        log::debug!("source {} steps at {time} us", self.source);
        let mut ctx = ctx::Ctx::<M>::new(
            ev,
            &mut self.throttle,
            self.source,
            time,
            reset,
            inputs,
            out,
            cmds,
        );
        self.module.step(&mut ctx);
    }
}
