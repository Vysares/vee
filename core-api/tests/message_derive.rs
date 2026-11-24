//! Tests of `#[derive(bus::Message)]` from outside the crate, as a real user sees it.
//! (Inside `core-api` itself the generated `::vee_core_api` paths would not resolve.)

use vee_core_api::bus;
use vee_core_api::message;

#[derive(Debug, Clone, Copy, PartialEq, bus::Message)]
struct Reading {
    volts: f32,
    count: u32,
    ok: bool,
}

#[test]
fn wire_len_is_the_sum_of_the_fields_with_no_padding() {
    assert_eq!(Reading::WIRE_LEN, 4 + 4 + 1);
}

#[test]
fn layout_has_one_entry_per_field_in_declaration_order_with_wire_offsets() {
    assert_eq!(
        Reading::LAYOUT,
        &[
            message::Field {
                name: "volts",
                offset: 0,
                kind: message::Kind::F32,
            },
            message::Field {
                name: "count",
                offset: 4,
                kind: message::Kind::U32,
            },
            message::Field {
                name: "ok",
                offset: 8,
                kind: message::Kind::Bool,
            },
        ]
    );
}

#[test]
fn encode_writes_big_endian_bytes_in_declaration_order() {
    let reading = Reading {
        volts: 1.5,
        count: 0x0102_0304,
        ok: true,
    };
    let mut expected = [0u8; Reading::WIRE_LEN];
    expected[0..4].copy_from_slice(&1.5_f32.to_be_bytes());
    expected[4..8].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    expected[8] = 1;
    assert_eq!(reading.encode(), expected);
}

#[test]
fn a_false_bool_encodes_as_zero() {
    let reading = Reading {
        volts: 0.0,
        count: 0,
        ok: false,
    };
    assert_eq!(reading.encode()[8], 0);
}

#[test]
fn encoded_fields_round_trip_through_read_by_layout() {
    let reading = Reading {
        volts: -2.5,
        count: 9,
        ok: true,
    };
    let bytes = reading.encode();
    assert_eq!(
        message::read(&bytes, &Reading::LAYOUT[0]),
        Some(message::Value::Float(f64::from(-2.5_f32)))
    );
    assert_eq!(
        message::read(&bytes, &Reading::LAYOUT[1]),
        Some(message::Value::Int(9))
    );
    assert_eq!(
        message::read(&bytes, &Reading::LAYOUT[2]),
        Some(message::Value::Bool(true))
    );
}

#[test]
fn a_message_with_no_fields_has_an_empty_layout_and_encodes_to_nothing() {
    #[derive(Debug, Clone, Copy, PartialEq, bus::Message)]
    struct Empty {}

    assert_eq!(Empty::WIRE_LEN, 0);
    assert_eq!(Empty::LAYOUT, &[]);
    assert_eq!(Empty {}.encode(), [0u8; 0]);
}

#[test]
fn a_message_can_be_used_as_a_queued_topics_message_type() {
    let mut topic = bus::QueuedTopic::<Reading, 1, 2>::new([bus::Policy::Drop]);
    let reading = Reading {
        volts: 5.0,
        count: 1,
        ok: true,
    };
    topic.publish(reading).unwrap();
    assert_eq!(topic.pop(0), Some(reading));
}
