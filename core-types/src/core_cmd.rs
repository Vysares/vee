//! The core's own commands.
//!
//! Made the same way a module's will be: one `#[derive(cmd::Command)]` struct per command's
//! args, and one `#[derive(cmd::CmdSet)]` enum wrapping them for `cmd::RawCmd` dispatch (see
//! `vee_core_api::cmd`). A variant's own discriminant is its opcode — the args struct it wraps
//! has no opcode of its own, since it is not itself a command, only the command's payload. Who
//! calls what: `core::commands::CoreCommands::accept` calls `CoreCmd::decode` then
//! `.validate()` before queuing; `CoreCommands::step` matches on the decoded `CoreCmd` and acts
//! on it.

use vee_core_api::cmd;

/// `SetEventFilter`'s args: the new bus-queue severity floor (`vee_core_api::ev::Ev::set_filter`).
/// `floor` is a plain `u8` so its wire size is explicit, not hidden inside an enum's own
/// representation; `Floor` names what each of its legal values means.
#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
#[validate]
pub struct SetEventFilterArgs {
    pub floor: u8,
}

impl SetEventFilterArgs {
    fn validate(&self) -> Result<(), cmd::CmdErr> {
        if Floor::try_from(self.floor).is_ok() {
            Ok(())
        } else {
            Err(cmd::CmdErr::Invalid("floor must be 0..=3"))
        }
    }
}

/// The named values `SetEventFilterArgs::floor` can wire-encode. This exists so a reader of this
/// file — ground, or whatever later exports these definitions into a command database — can see
/// what each byte value means without that meaning being buried in a match somewhere. What each
/// value actually does to `Ev`'s filter is flight-software's business, not this file's: see
/// `core::commands::CoreCommands::step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Floor {
    Off = 0,
    Info = 1,
    Warn = 2,
    Err = 3,
}

impl TryFrom<u8> for Floor {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Floor::Off),
            1 => Ok(Floor::Info),
            2 => Ok(Floor::Warn),
            3 => Ok(Floor::Err),
            _ => Err(()),
        }
    }
}

/// `Noop`'s args: none. The core does nothing and reports success, so a sender can check that a
/// command reaches the core and comes back acknowledged (the router's "accepted", then the
/// core's "succeeded"), end to end, without changing anything.
#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
pub struct NoopArgs {}

/// The core's commands, dispatched by `cmd::RawCmd::opcode`. See `vee_core_api::cmd` for how a
/// target's queue stores and drains one of these.
#[derive(Debug, Clone, Copy, PartialEq, cmd::CmdSet)]
#[repr(u16)]
pub enum CoreCmd {
    SetEventFilter(SetEventFilterArgs) = 1,
    Noop(NoopArgs) = 2,
}

#[cfg(test)]
mod tests {
    use super as core_cmd;
    use vee_core_api::cmd;

    // `cmd::CORE_TARGET` has to be a plain constant, not a re-export of `core_event::SOURCE`,
    // because `core-api` cannot depend on `core-types` (see `core-api/src/cmd.rs`'s module doc
    // comment). The two literals agreeing is the whole load-bearing fact, so it is checked here.
    #[test]
    fn the_core_is_the_same_target_id_as_its_own_event_source() {
        assert_eq!(cmd::CORE_TARGET, crate::core_event::SOURCE);
    }

    #[test]
    fn noop_is_opcode_two_and_takes_no_args() {
        let raw = cmd::RawCmd {
            sink: 1,
            seq: 1,
            target: cmd::CORE_TARGET,
            opcode: 2,
            args: cmd::Args::EMPTY,
        };
        let decoded = core_cmd::CoreCmd::decode(&raw).unwrap();
        assert_eq!(decoded, core_cmd::CoreCmd::Noop(core_cmd::NoopArgs {}));
        assert_eq!(decoded.validate(), Ok(()));
        assert_eq!(core_cmd::NoopArgs::WIRE_LEN, 0);
    }

    #[test]
    fn floor_try_from_maps_each_legal_byte_to_its_named_value() {
        assert_eq!(core_cmd::Floor::try_from(0), Ok(core_cmd::Floor::Off));
        assert_eq!(core_cmd::Floor::try_from(1), Ok(core_cmd::Floor::Info));
        assert_eq!(core_cmd::Floor::try_from(2), Ok(core_cmd::Floor::Warn));
        assert_eq!(core_cmd::Floor::try_from(3), Ok(core_cmd::Floor::Err));
    }

    #[test]
    fn floor_try_from_rejects_anything_above_three() {
        assert_eq!(core_cmd::Floor::try_from(4), Err(()));
    }

    #[test]
    fn validate_accepts_zero_through_three() {
        for floor in 0..=3 {
            assert_eq!(core_cmd::SetEventFilterArgs { floor }.validate(), Ok(()));
        }
    }

    #[test]
    fn validate_rejects_anything_above_three() {
        assert_eq!(
            core_cmd::SetEventFilterArgs { floor: 4 }.validate(),
            Err(vee_core_api::cmd::CmdErr::Invalid("floor must be 0..=3"))
        );
    }
}
