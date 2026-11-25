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

// ---- Every kind of field, at the edges of its range ----

#[derive(Debug, Clone, Copy, PartialEq, bus::Message)]
struct Everything {
    a: bool,
    b: u8,
    c: u16,
    d: u32,
    e: u64,
    f: i8,
    g: i16,
    h: i32,
    i: i64,
    j: f32,
    k: f64,
}

const SMALLEST: Everything = Everything {
    a: false,
    b: 0,
    c: 0,
    d: 0,
    e: 0,
    f: i8::MIN,
    g: i16::MIN,
    h: i32::MIN,
    i: i64::MIN,
    j: f32::MIN,
    k: f64::MIN,
};

const LARGEST: Everything = Everything {
    a: true,
    b: u8::MAX,
    c: u16::MAX,
    d: u32::MAX,
    e: u64::MAX,
    f: i8::MAX,
    g: i16::MAX,
    h: i32::MAX,
    i: i64::MAX,
    j: f32::MAX,
    k: f64::MAX,
};

#[test]
fn a_message_of_every_kind_has_each_field_at_the_sum_of_the_widths_before_it() {
    assert_eq!(
        Everything::WIRE_LEN,
        1 + 1 + 2 + 4 + 8 + 1 + 2 + 4 + 8 + 4 + 8
    );
    let offsets: Vec<usize> = Everything::LAYOUT
        .iter()
        .map(|field| field.offset)
        .collect();
    assert_eq!(offsets, [0, 1, 2, 4, 8, 16, 17, 19, 23, 31, 35]);
    let kinds: Vec<message::Kind> = Everything::LAYOUT.iter().map(|field| field.kind).collect();
    assert_eq!(
        kinds,
        [
            message::Kind::Bool,
            message::Kind::U8,
            message::Kind::U16,
            message::Kind::U32,
            message::Kind::U64,
            message::Kind::I8,
            message::Kind::I16,
            message::Kind::I32,
            message::Kind::I64,
            message::Kind::F32,
            message::Kind::F64,
        ]
    );
}

#[test]
fn the_smallest_and_largest_value_of_every_kind_read_back_exactly() {
    for message in [SMALLEST, LARGEST] {
        let bytes = message.encode();
        let read = |index: usize| message::read(&bytes, &Everything::LAYOUT[index]).unwrap();
        assert_eq!(read(0), message::Value::Bool(message.a));
        assert_eq!(read(1), message::Value::Int(i128::from(message.b)));
        assert_eq!(read(2), message::Value::Int(i128::from(message.c)));
        assert_eq!(read(3), message::Value::Int(i128::from(message.d)));
        assert_eq!(read(4), message::Value::Int(i128::from(message.e)));
        assert_eq!(read(5), message::Value::Int(i128::from(message.f)));
        assert_eq!(read(6), message::Value::Int(i128::from(message.g)));
        assert_eq!(read(7), message::Value::Int(i128::from(message.h)));
        assert_eq!(read(8), message::Value::Int(i128::from(message.i)));
        assert_eq!(read(9), message::Value::Float(f64::from(message.j)));
        assert_eq!(read(10), message::Value::Float(message.k));
    }
}

#[test]
fn encode_into_needs_exactly_the_messages_size_and_writes_nothing_when_short() {
    use message::Message as _;
    let mut exact = [0u8; Everything::WIRE_LEN];
    assert_eq!(LARGEST.encode_into(&mut exact), Some(Everything::WIRE_LEN));
    assert_eq!(exact, LARGEST.encode());

    // One byte short: `None`, and not a byte of the buffer touched.
    let mut short = [0x5A_u8; Everything::WIRE_LEN - 1];
    assert_eq!(LARGEST.encode_into(&mut short), None);
    assert_eq!(short, [0x5A; Everything::WIRE_LEN - 1]);
    assert_eq!(LARGEST.encode_into(&mut []), None);

    // One byte spare: only the message's own bytes are written.
    let mut spare = [0x5A_u8; Everything::WIRE_LEN + 1];
    assert_eq!(LARGEST.encode_into(&mut spare), Some(Everything::WIRE_LEN));
    assert_eq!(spare[Everything::WIRE_LEN], 0x5A);
}
