//! Tests of `#[derive(cmd::Command)]` from outside the crate, as a real user sees it.
//! (Inside `core-api` itself the generated `::vee_core_api` paths would not resolve, the same
//! note as `message_derive.rs` and `code_derive.rs`.)

use vee_core_api::cmd;

#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
struct SetLimitArgs {
    max: u16,
    enabled: bool,
}

#[test]
fn wire_len_is_the_sum_of_the_fields_with_no_padding() {
    assert_eq!(SetLimitArgs::WIRE_LEN, 2 + 1);
}

#[test]
fn encode_writes_big_endian_bytes_in_declaration_order() {
    let cmd = SetLimitArgs {
        max: 0x0102,
        enabled: true,
    };
    assert_eq!(cmd.encode(), [0x01, 0x02, 1]);
}

#[test]
fn a_false_bool_encodes_as_zero() {
    let cmd = SetLimitArgs {
        max: 0,
        enabled: false,
    };
    assert_eq!(cmd.encode(), [0, 0, 0]);
}

#[test]
fn decode_is_the_reverse_of_encode() {
    let cmd = SetLimitArgs {
        max: 0x0304,
        enabled: true,
    };
    assert_eq!(SetLimitArgs::decode(&cmd.encode()), Ok(cmd));
}

#[test]
fn decode_with_bytes_shorter_than_wire_len_is_bad_args() {
    assert_eq!(
        SetLimitArgs::decode(&[1, 2]),
        Err(vee_core_api::cmd::CmdErr::BadArgs)
    );
}

#[test]
fn decode_ignores_bytes_after_wire_len() {
    let cmd = SetLimitArgs {
        max: 9,
        enabled: false,
    };
    let mut bytes = cmd.encode().to_vec();
    bytes.push(0xFF);
    assert_eq!(SetLimitArgs::decode(&bytes), Ok(cmd));
}

#[test]
fn without_validate_the_derive_generates_one_that_always_succeeds() {
    let cmd = SetLimitArgs {
        max: 0,
        enabled: false,
    };
    assert_eq!(cmd.validate(), Ok(()));
}

#[test]
fn a_command_with_no_fields_has_zero_wire_len_and_encodes_to_nothing() {
    #[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
    struct PingArgs {}

    assert_eq!(PingArgs::WIRE_LEN, 0);
    assert_eq!(PingArgs {}.encode(), [0u8; 0]);
    assert_eq!(PingArgs::decode(&[]), Ok(PingArgs {}));
}

#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
#[validate]
struct SetFloorArgs {
    floor: u8,
}

impl SetFloorArgs {
    fn validate(&self) -> Result<(), cmd::CmdErr> {
        if self.floor <= 3 {
            Ok(())
        } else {
            Err(cmd::CmdErr::Invalid("floor must be 0..=3"))
        }
    }
}

#[test]
fn with_validate_the_derive_leaves_the_hand_written_one_in_place() {
    assert_eq!(SetFloorArgs { floor: 3 }.validate(), Ok(()));
    assert_eq!(
        SetFloorArgs { floor: 4 }.validate(),
        Err(cmd::CmdErr::Invalid("floor must be 0..=3"))
    );
}
