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

#[cfg(test)]
mod tests {
    use super as pinger;
    use vee_core_api::cmd;
    use vee_core_api::ctx;
    use vee_core_api::ev;
    use vee_core_api::module::Module as _;
    use vee_core_api::reset;
    use vee_core_api::throttle;

    const SOURCE: u8 = 3;

    /// One step of `pinger` on source `SOURCE`, with `sink` to submit into.
    fn step(pinger: &mut pinger::Pinger, sink: &mut cmd::Sink, ev: &mut ev::Ev) {
        let mut throttle = throttle::Throttle::new();
        let mut ctx = ctx::Ctx::<pinger::Pinger>::new(
            ev,
            &mut throttle,
            SOURCE,
            0,
            reset::ResetInfo::first_boot(),
            (),
            pinger::PingerOut { cmd: sink.handle() },
            cmd::Inbox::empty(),
        );
        pinger.step(&mut ctx);
    }

    #[test]
    fn a_step_submits_one_noop_to_the_core_with_the_next_seq() {
        let mut pinger = pinger::Pinger::new();
        let mut sink = cmd::Sink::new(9);
        let mut ev = ev::Ev::new();
        step(&mut pinger, &mut sink, &mut ev);
        step(&mut pinger, &mut sink, &mut ev);

        let first = sink.pop().unwrap();
        assert_eq!(
            (first.sink, first.seq, first.target, first.opcode),
            (9, 0, cmd::CORE_TARGET, 2)
        );
        assert!(first.args.is_empty(), "a Noop has no args");
        assert_eq!(sink.pop().unwrap().seq, 1);
        assert!(sink.pop().is_none());
        assert!(
            ev.log.is_empty(),
            "nothing went wrong, so nothing is reported"
        );
    }

    #[test]
    fn the_seq_wraps_round_at_the_top_of_a_u16_and_does_not_panic() {
        let mut pinger = pinger::Pinger::new();
        pinger.seq = u16::MAX;
        let mut sink = cmd::Sink::new(1);
        let mut ev = ev::Ev::new();
        step(&mut pinger, &mut sink, &mut ev);
        step(&mut pinger, &mut sink, &mut ev);
        assert_eq!(sink.pop().unwrap().seq, u16::MAX);
        assert_eq!(sink.pop().unwrap().seq, 0);
    }

    #[test]
    fn a_full_sink_is_reported_and_the_pinger_carries_on() {
        let mut pinger = pinger::Pinger::new();
        let mut sink = cmd::Sink::new(1);
        let mut ev = ev::Ev::new();
        for _ in 0..cmd::CMD_SINK_QUEUE_DEPTH {
            step(&mut pinger, &mut sink, &mut ev);
        }
        assert!(ev.log.is_empty(), "the sink took exactly its depth");

        // One more than the sink holds: refused, and said so.
        step(&mut pinger, &mut sink, &mut ev);
        let event = ev.log.pop().unwrap();
        assert_eq!(event.source, SOURCE);
        assert_eq!(event.severity, vee_core_api::event::Severity::Warn);
        assert_eq!(
            event.text.as_str(),
            format!(
                "noop {} not sent: command queue is full",
                cmd::CMD_SINK_QUEUE_DEPTH
            )
        );

        // Once the sink has room, the next step goes in.
        sink.pop().unwrap();
        step(&mut pinger, &mut sink, &mut ev);
        assert!(ev.log.is_empty());
    }
}
