//! Tests of `#[derive(cmd::CmdSet)]` from outside the crate, as a real user sees it.
//! (Inside `core-api` itself the generated `::vee_core_api` paths would not resolve, the same
//! note as `command_derive.rs`, `message_derive.rs` and `code_derive.rs`.)

use vee_core_api::cmd;

#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
struct ActivateArgs {
    slot: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
#[validate]
struct SetLimitArgs {
    max_rts: u16,
}

impl SetLimitArgs {
    fn validate(&self) -> Result<(), cmd::CmdErr> {
        if self.max_rts > 0 {
            Ok(())
        } else {
            Err(cmd::CmdErr::Invalid("max_rts must be nonzero"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, cmd::CmdSet)]
#[repr(u16)]
enum AtsCmd {
    Activate(ActivateArgs) = 1,
    SetLimit(SetLimitArgs) = 2,
}

// Each test below builds its own `RawCmd` rather than calling a shared helper, so its
// `.unwrap()` stays directly inside a `#[test]` function: `clippy.toml`'s
// `allow-unwrap-in-tests` only recognizes unwraps there, not in a helper a test merely calls
// (same as `dyn_view.rs` and `message_derive.rs`).

#[test]
fn decode_matches_the_opcode_to_its_variant() {
    let raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 1,
        args: cmd::Args::new(&[5]).unwrap(),
    };
    let decoded = AtsCmd::decode(&raw).unwrap();
    assert_eq!(decoded, AtsCmd::Activate(ActivateArgs { slot: 5 }));
}

#[test]
fn decode_finds_a_later_variant_too() {
    let raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 2,
        args: cmd::Args::new(&[0x00, 0x0A]).unwrap(),
    };
    let decoded = AtsCmd::decode(&raw).unwrap();
    assert_eq!(decoded, AtsCmd::SetLimit(SetLimitArgs { max_rts: 10 }));
}

#[test]
fn decode_with_an_unknown_opcode_is_unknown_opcode() {
    let raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 0xFF,
        args: cmd::Args::new(&[]).unwrap(),
    };
    assert_eq!(AtsCmd::decode(&raw), Err(cmd::CmdErr::UnknownOpcode));
}

#[test]
fn decode_passes_on_the_variants_own_bad_args() {
    let raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 1, // Activate needs 1 byte
        args: cmd::Args::new(&[]).unwrap(),
    };
    assert_eq!(AtsCmd::decode(&raw), Err(cmd::CmdErr::BadArgs));
}

#[test]
fn validate_delegates_to_the_decoded_variants_own_validate() {
    let ok_raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 2,
        args: cmd::Args::new(&[0x00, 0x0A]).unwrap(),
    };
    let ok = AtsCmd::decode(&ok_raw).unwrap();
    assert_eq!(ok.validate(), Ok(()));

    let bad_raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 2,
        args: cmd::Args::new(&[0x00, 0x00]).unwrap(),
    };
    let bad = AtsCmd::decode(&bad_raw).unwrap();
    assert_eq!(
        bad.validate(),
        Err(cmd::CmdErr::Invalid("max_rts must be nonzero"))
    );
}

#[test]
fn validate_on_a_variant_with_no_hand_written_validate_always_succeeds() {
    let raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: 1,
        args: cmd::Args::new(&[0]).unwrap(),
    };
    let decoded = AtsCmd::decode(&raw).unwrap();
    assert_eq!(decoded.validate(), Ok(()));
}

#[test]
fn opcode_is_the_variants_own_discriminant_and_matches_what_decode_accepts() {
    let activate = AtsCmd::Activate(ActivateArgs { slot: 5 });
    let set_limit = AtsCmd::SetLimit(SetLimitArgs { max_rts: 10 });
    assert_eq!(activate.opcode(), 1);
    assert_eq!(set_limit.opcode(), 2);

    // A sender builds the raw command from `opcode()` and `encode()`, and the target decodes it
    // back to the same command.
    let raw = cmd::RawCmd {
        sink: 1,
        seq: 1,
        target: 9,
        opcode: set_limit.opcode(),
        args: cmd::Args::new(&SetLimitArgs { max_rts: 10 }.encode()).unwrap(),
    };
    assert_eq!(AtsCmd::decode(&raw).unwrap(), set_limit);
}
