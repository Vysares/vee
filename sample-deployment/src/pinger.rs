//! The sample's stand-in for the link module: a module whose only job is to send the core a
//! `Noop` command, through a command sink, so the sample exercises the whole command path (the
//! sink, the router, the core's queue, the core's handler, and both acks) in the real binary.
//!
//! The real link module will read commands from a ground link and submit them through the same
//! sink handle the same way. Nothing here is special to the sample except where the command
//! comes from.

use vee_core_api::cmd;
use vee_core_api::ctx;
use vee_core_api::event;
use vee_core_api::module;
use vee_core_types::core_cmd;

/// The pinger's own events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
pub enum PingerEv {
    /// The command could not be sent: the sink was full, or the args did not fit.
    #[severity(Warn)]
    NotSent,
}

/// What the pinger writes: the command sink.
pub struct PingerOut<'a> {
    pub cmd: cmd::SinkHandle<'a>,
}

pub struct Pinger {
    /// The `seq` of the next command: the sender's own number, which comes back in the acks.
    seq: u16,
}

impl Pinger {
    pub fn new() -> Self {
        Pinger { seq: 0 }
    }
}

impl Default for Pinger {
    fn default() -> Self {
        Self::new()
    }
}

impl module::Module for Pinger {
    type Event = PingerEv;
    type In<'a> = ();
    type Out<'a> = PingerOut<'a>;
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);

        let command = core_cmd::CoreCmd::Noop(core_cmd::NoopArgs {});
        let result = cmd::Args::new(&core_cmd::NoopArgs {}.encode()).and_then(|args| {
            ctx.out
                .cmd
                .submit(seq, cmd::CORE_TARGET, command.opcode(), args)
        });
        if let Err(error) = result {
            ctx.ev.send(
                PingerEv::NotSent,
                format_args!("noop {seq} not sent: {error}"),
            );
        }
    }
}
