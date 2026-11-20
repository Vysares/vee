//! `Slot<M>`: everything the executive keeps for one module, and the three things it does with it
//! (`init`, `report_suppressed`, `step`). It is the same for every module, so it is written
//! once, here, and the deployment's executive only holds one `Slot` for each module and calls
//! them in order. (That is all `exec!` will have to generate.)
//!
//! A module never sees any of this: it sees the `Ctx` that a `Slot` builds for each call.

use crate::suppressed;
use vee_core_api::ctx;
use vee_core_api::ev;
use vee_core_api::module;
use vee_core_api::reset;
use vee_core_api::throttle;
use vee_core_types::core_event;
use vee_osal::clock;

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

    /// Startup: reads the clock, builds the module's `Ctx` and calls `init`.
    pub fn init(&mut self, ev: &mut ev::Ev, clock: &impl clock::Clock, reset: reset::ResetInfo) {
        let time = clock.monotonic_us();
        log::debug!("source {} init at {time} us", self.source);
        let mut ctx = ctx::Ctx::<M>::new(ev, &mut self.throttle, self.source, time, reset);
        self.module.init(&mut ctx);
    }

    /// Top of a tick: sends the core's report of what this module had suppressed in the tick
    /// before, as events from the core at `now`. It also starts the module's new tick of counting.
    pub fn report_suppressed(&mut self, ev: &mut ev::Ev, now: u64) {
        suppressed::report::<M::Event, _>(&mut self.throttle, self.source, |code, text| {
            ev.send(core_event::SOURCE, now, code, text);
        });
    }

    /// Calls `step` if this tick is one of the module's. The clock is read just before the call.
    pub fn step(
        &mut self,
        tick: u64,
        ev: &mut ev::Ev,
        clock: &impl clock::Clock,
        reset: reset::ResetInfo,
    ) {
        if tick.checked_rem(self.period) != Some(0) {
            return;
        }
        let time = clock.monotonic_us();
        log::debug!("source {} steps at {time} us", self.source);
        let mut ctx = ctx::Ctx::<M>::new(ev, &mut self.throttle, self.source, time, reset);
        self.module.step(&mut ctx);
    }
}
