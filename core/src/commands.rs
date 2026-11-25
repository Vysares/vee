//! The core's own commands: its command queue, and what each command does.
//!
//! `CoreCommands` is the core's `cmd::CommandTarget`, the same shape a module's will have:
//! - `accept` (called by `cmd::route`, when a command arrives) decodes the `RawCmd` with the
//!   `CoreCmd` enum, validates it, and queues it with its `CmdMeta`. It runs nothing.
//!
//! There is no `step` here. Like a module, the core drains its own queue from its own step
//! (`Core::tick`): `cmd::drain(&mut commands.queue, ev, now, commands::handle)`. `drain` pops each
//! command, calls `handle` below, and sends the outcome event itself, so nothing here writes ack
//! or nak text.

use vee_core_api::cmd;
use vee_core_api::config;
use vee_core_api::ev;
use vee_core_api::event;
use vee_core_api::queue;
use vee_core_types::core_cmd;

config::setting! {
    /// How many commands the core's own queue holds. A command waits here from `route` until the
    /// next `step`, which is the next tick, so this only has to cover the commands that arrive
    /// in one tick. Default 8, 1..=65535.
    pub CORE_CMD_QUEUE_DEPTH: usize = 8, 1..=65535;
}

/// The core's command queue. Each entry is a decoded command and what `drain` needs to report
/// its outcome.
pub struct CoreCommands {
    /// Read by `Core::tick`, which drains it (see the module comment).
    pub(crate) queue: queue::Queue<(core_cmd::CoreCmd, cmd::CmdMeta), CORE_CMD_QUEUE_DEPTH>,
}

impl CoreCommands {
    pub const fn new() -> Self {
        CoreCommands {
            queue: queue::Queue::new(),
        }
    }
}

impl Default for CoreCommands {
    fn default() -> Self {
        Self::new()
    }
}

impl cmd::CommandTarget for CoreCommands {
    /// Decodes and validates, then queues. A full queue gives `QueueFull`, and the command is
    /// not run.
    fn accept(&mut self, raw: &cmd::RawCmd) -> Result<(), cmd::CmdErr> {
        let command = core_cmd::CoreCmd::decode(raw)?;
        command.validate()?;
        self.queue
            .push((command, cmd::CmdMeta::from(raw)))
            .map_err(|_| cmd::CmdErr::QueueFull)
    }
}

/// What each core command does. This is the only place the meaning of a command lives:
/// `core_cmd` only says what is legal.
pub(crate) fn handle(command: core_cmd::CoreCmd, ev: &mut ev::Ev) -> cmd::CmdResult {
    match command {
        core_cmd::CoreCmd::SetEventFilter(args) => match core_cmd::Floor::try_from(args.floor) {
            Ok(floor) => {
                ev.set_filter(severity_floor(floor));
                cmd::CmdResult::Success
            }
            // `accept` already validated this, so it cannot happen; it is still an outcome, not
            // a panic.
            Err(()) => cmd::CmdResult::Fail("floor out of range"),
        },
        // Does nothing, on purpose: the outcome event `drain` sends is the whole point.
        core_cmd::CoreCmd::Noop(core_cmd::NoopArgs {}) => cmd::CmdResult::Success,
    }
}

/// The bus queue's floor for a `Floor`: `Off` is no filter at all (`None`), the others let that
/// severity and worse through.
fn severity_floor(floor: core_cmd::Floor) -> Option<event::Severity> {
    match floor {
        core_cmd::Floor::Off => None,
        core_cmd::Floor::Info => Some(event::Severity::Info),
        core_cmd::Floor::Warn => Some(event::Severity::Warn),
        core_cmd::Floor::Err => Some(event::Severity::Err),
    }
}

#[cfg(test)]
mod tests {
    use super as commands;
    use vee_core_api::cmd;
    use vee_core_api::cmd::CommandTarget as _;
    use vee_core_api::ev;
    use vee_core_api::event;
    use vee_core_types::core_event;

    const SET_EVENT_FILTER: u16 = 1;
    const NOOP: u16 = 2;

    fn raw(opcode: u16, bytes: &[u8]) -> cmd::RawCmd {
        cmd::RawCmd {
            sink: 1,
            seq: 7,
            target: cmd::CORE_TARGET,
            opcode,
            args: cmd::Args::new(bytes).unwrap(),
        }
    }

    /// Whether an event of `severity` sent now reaches the bus queue, i.e. whether the filter
    /// lets it through. The bus queue is emptied first (a command's own outcome event can be
    /// sitting in it) and the probe is taken back out, so it is left empty.
    fn passes_filter(ev: &mut ev::Ev, severity: event::Severity) -> bool {
        while ev.bus.pop().is_some() {}
        let code = match severity {
            event::Severity::Info => core_event::CoreEvent::ResetReport,
            event::Severity::Warn => core_event::CoreEvent::ResetLogPartialRecord,
            event::Severity::Err => core_event::CoreEvent::ResetLogCorruptRecord,
        };
        ev.send(core_event::SOURCE, 0, code, format_args!("probe"));
        ev.bus.pop().is_some()
    }

    #[test]
    fn accept_queues_without_running_anything() {
        let mut core_commands = commands::CoreCommands::new();
        let mut ev = ev::Ev::new();
        assert_eq!(core_commands.accept(&raw(SET_EVENT_FILTER, &[1])), Ok(()));
        // Nothing ran: no outcome event, and the filter is still off.
        assert!(ev.log.is_empty());
        assert!(!passes_filter(&mut ev, event::Severity::Err));
    }

    #[test]
    fn step_sets_the_filter_and_reports_success() {
        let mut core_commands = commands::CoreCommands::new();
        let mut ev = ev::Ev::new();
        core_commands.accept(&raw(SET_EVENT_FILTER, &[2])).unwrap();
        cmd::drain(&mut core_commands.queue, &mut ev, 10, commands::handle);

        let outcome = ev.log.pop().unwrap();
        assert_eq!(outcome.text.as_bytes(), b"cmd 1/7 t0 op0x0001: succeeded");
        assert_eq!(outcome.monotonic_us, 10);
        assert!(ev.log.is_empty());

        // Floor 2 is Warn: Info stays out, Warn and Err go in.
        assert!(!passes_filter(&mut ev, event::Severity::Info));
        assert!(passes_filter(&mut ev, event::Severity::Warn));
        assert!(passes_filter(&mut ev, event::Severity::Err));
    }

    #[test]
    fn floor_zero_turns_the_filter_off() {
        let mut core_commands = commands::CoreCommands::new();
        let mut ev = ev::Ev::new();
        ev.set_filter(Some(event::Severity::Info));
        core_commands.accept(&raw(SET_EVENT_FILTER, &[0])).unwrap();
        cmd::drain(&mut core_commands.queue, &mut ev, 10, commands::handle);
        assert!(!passes_filter(&mut ev, event::Severity::Err));
    }

    #[test]
    fn a_noop_succeeds_and_changes_nothing() {
        let mut core_commands = commands::CoreCommands::new();
        let mut ev = ev::Ev::new();
        ev.set_filter(Some(event::Severity::Warn));
        core_commands.accept(&raw(NOOP, &[])).unwrap();
        cmd::drain(&mut core_commands.queue, &mut ev, 10, commands::handle);

        let outcome = ev.log.pop().unwrap();
        assert_eq!(outcome.text.as_bytes(), b"cmd 1/7 t0 op0x0002: succeeded");
        assert!(ev.log.is_empty());
        // The filter is as it was: Warn still passes, Info still does not.
        assert!(!passes_filter(&mut ev, event::Severity::Info));
        assert!(passes_filter(&mut ev, event::Severity::Warn));
    }

    #[test]
    fn accept_refuses_an_unknown_opcode() {
        let mut core_commands = commands::CoreCommands::new();
        assert_eq!(
            core_commands.accept(&raw(99, &[])),
            Err(cmd::CmdErr::UnknownOpcode)
        );
    }

    #[test]
    fn accept_refuses_args_that_are_too_short() {
        let mut core_commands = commands::CoreCommands::new();
        assert_eq!(
            core_commands.accept(&raw(SET_EVENT_FILTER, &[])),
            Err(cmd::CmdErr::BadArgs)
        );
    }

    #[test]
    fn accept_refuses_a_floor_that_is_not_legal() {
        let mut core_commands = commands::CoreCommands::new();
        assert_eq!(
            core_commands.accept(&raw(SET_EVENT_FILTER, &[4])),
            Err(cmd::CmdErr::Invalid("floor must be 0..=3"))
        );
    }

    #[test]
    fn accept_refuses_when_the_queue_is_full() {
        let mut core_commands = commands::CoreCommands::new();
        for _ in 0..commands::CORE_CMD_QUEUE_DEPTH {
            core_commands.accept(&raw(SET_EVENT_FILTER, &[1])).unwrap();
        }
        assert_eq!(
            core_commands.accept(&raw(SET_EVENT_FILTER, &[1])),
            Err(cmd::CmdErr::QueueFull)
        );
    }

    #[test]
    fn step_runs_commands_in_the_order_they_arrived() {
        let mut core_commands = commands::CoreCommands::new();
        let mut ev = ev::Ev::new();
        core_commands.accept(&raw(SET_EVENT_FILTER, &[1])).unwrap();
        core_commands.accept(&raw(SET_EVENT_FILTER, &[3])).unwrap();
        cmd::drain(&mut core_commands.queue, &mut ev, 10, commands::handle);
        // The later command (floor 3, Err) won: only Err gets through.
        assert!(!passes_filter(&mut ev, event::Severity::Warn));
        assert!(passes_filter(&mut ev, event::Severity::Err));
    }
}
