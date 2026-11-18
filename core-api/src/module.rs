//! What a module is, as the executive sees it.
//!
//! A module is a struct that holds the module's own state, and implements `Module`. The
//! executive calls `init` once at startup and `step` at the module's rate, and gives each call
//! a `Ctx`, the module's only way to reach the rest of the system.
//!
//! Not here yet: `command()` (it waits for the command design), and the bus, tables and devices
//! (they will be more of what `Ctx` holds).

use crate::ctx;
use crate::event;

pub trait Module: Sized {
    /// The module's own events: an enum with `#[derive(event::Code)]`. A module can send only
    /// these (see `ctx::EvHandle::send`).
    type Event: event::Code;

    /// Runs once at startup, before the first tick. Use it to read `ctx.reset` (how the last run
    /// ended) and do other setup.
    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>);

    /// Runs at the module's rate. It does one step of the module's work: it must return, because
    /// the executive cannot interrupt it.
    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>);
}
