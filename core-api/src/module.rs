//! What a module is, as the executive sees it.
//!
//! A module is a struct that holds the module's own state, and implements `Module`. The
//! executive calls `init` once at startup and `step` at the module's rate, and gives each call
//! a `Ctx`, the module's only way to reach the rest of the system.
//!
//! A module also says which bus topics it reads (`In`) and writes (`Out`). Those are the handles
//! the executive puts in the module's `Ctx`, so what a module can reach is written in its type.
//!
//! A module handles its commands in its own `step`, with `ctx.drain_cmds`.
//!
//! Not here yet: tables and devices (they will be more of what `Ctx` holds).

use crate::ctx;
use crate::event;

pub trait Module: Sized {
    /// The module's own events: an enum with `#[derive(event::Code)]`. A module can send only
    /// these (see `ctx::EvHandle::send`).
    type Event: event::Code;

    /// The bus topics the module READS: a struct of readers, one for each topic, which the
    /// executive makes for each call and puts in `ctx.inputs`. A module that reads nothing says
    /// `type In<'a> = ();`.
    ///
    /// The `<'a>` makes this a generic associated type: a type that takes a lifetime, like
    /// `Ref<'a, T>` takes one in C++ terms. It is needed because a reader borrows a topic that
    /// the executive owns, and the borrow lasts only for one call (`'a`). A plain `type In;`
    /// could not name that borrow.
    type In<'a>;

    /// The bus topics the module WRITES: a struct of writers, in `ctx.out`. A module that writes
    /// nothing says `type Out<'a> = ();`.
    type Out<'a>;

    /// The commands the module handles: an enum with `#[derive(cmd::CmdSet)]`, whose variants each
    /// wrap a `#[derive(cmd::Command)]` struct. The executive owns the module's command queue
    /// (the router fills it) and the module drains it with `ctx.drain_cmds`. A module with no
    /// commands says `type Cmd = ();`.
    type Cmd: 'static;

    /// Runs once at startup, before the first tick. Use it to read `ctx.reset` (how the last run
    /// ended) and do other setup.
    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>);

    /// Runs at the module's rate. It does one step of the module's work: it must return, because
    /// the executive cannot interrupt it.
    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>);
}
