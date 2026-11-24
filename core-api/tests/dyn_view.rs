//! Tests of `bus::Topic`, the dyn view: what a module that picks topics by table (telemetry,
//! FDIR, the recorder) can do with ANY topic, through one interface, whatever its message type.
//! Outside the crate because the derive's `::vee_core_api` paths would not resolve inside it,
//! same as `code_derive.rs` and `message_derive.rs`.

use vee_core_api::bus;
use vee_core_api::bus::Topic as _;
use vee_core_api::message;

#[derive(Debug, Clone, Copy, PartialEq, bus::Message)]
struct Reading {
    volts: f32,
    ok: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, bus::Message)]
struct Status {
    armed: bool,
}

#[test]
fn a_state_topic_never_published_has_no_latest_bytes_or_field() {
    let topic = bus::StateTopic::<Reading>::new();
    let mut buf = [0u8; 16];
    assert_eq!(topic.latest_bytes(&mut buf), None);
    assert_eq!(topic.read_latest(0), None);
}

#[test]
fn a_state_topics_latest_bytes_are_the_same_as_encode() {
    let mut topic = bus::StateTopic::new();
    let reading = Reading {
        volts: 3.5,
        ok: true,
    };
    topic.publish(0, reading);

    let mut buf = [0u8; 16];
    let len = topic.latest_bytes(&mut buf).unwrap();
    assert_eq!(buf[..len], reading.encode());
}

#[test]
fn a_too_small_buffer_is_none_not_a_partial_write() {
    let mut topic = bus::StateTopic::new();
    topic.publish(
        0,
        Reading {
            volts: 1.0,
            ok: false,
        },
    );
    let mut buf = [0u8; 1]; // Reading::WIRE_LEN is 5
    assert_eq!(topic.latest_bytes(&mut buf), None);
}

#[test]
fn read_latest_reads_the_right_field_by_index() {
    let mut topic = bus::StateTopic::new();
    let reading = Reading {
        volts: -2.0,
        ok: true,
    };
    topic.publish(0, reading);

    assert_eq!(
        topic.read_latest(0),
        Some(message::Value::Float(f64::from(-2.0_f32)))
    );
    assert_eq!(topic.read_latest(1), Some(message::Value::Bool(true)));
}

#[test]
fn read_latest_with_an_out_of_range_index_is_none() {
    let mut topic = bus::StateTopic::new();
    topic.publish(
        0,
        Reading {
            volts: 1.0,
            ok: true,
        },
    );
    assert_eq!(topic.read_latest(2), None); // Reading::LAYOUT has 2 fields: 0 and 1
}

#[test]
fn a_queued_topic_has_no_latest_only_its_queue() {
    let mut topic = bus::QueuedTopic::<Reading, 1, 2>::new([bus::Policy::Drop]);
    let reading = Reading {
        volts: 9.0,
        ok: false,
    };
    topic.publish(reading).unwrap();

    // No "latest" side: a queued topic only has `pop_encoded`.
    let mut buf = [0u8; 16];
    assert_eq!(bus::Topic::latest_bytes(&topic, &mut buf), None);
    assert_eq!(bus::Topic::read_latest(&topic, 0), None);

    let len = topic.pop_encoded(0, &mut buf).unwrap();
    assert_eq!(buf[..len], reading.encode());
    assert_eq!(topic.pop_encoded(0, &mut buf), None); // nothing left waiting
}

#[test]
fn find_locates_a_topic_by_tid_in_a_sorted_list() {
    let state = bus::StateTopic::<Reading>::new();
    let queued = bus::QueuedTopic::<Reading, 1, 2>::new([bus::Policy::Drop]);
    let topics: [(bus::Tid, &dyn bus::Topic); 2] = [(0x10, &state), (0x20, &queued)];

    assert!(core::ptr::eq(
        bus::find(&topics, 0x10).unwrap(),
        &state as &dyn bus::Topic,
    ));
    assert!(core::ptr::eq(
        bus::find(&topics, 0x20).unwrap(),
        &queued as &dyn bus::Topic,
    ));
    assert!(bus::find(&topics, 0x30).is_none());
}

#[test]
fn a_consumer_that_looks_up_a_topic_by_tid_reads_that_topics_own_data() {
    // The actual path telemetry/FDIR will use: a table row gives a TID, `find` resolves it to a
    // `&dyn Topic`, and only THEN does a field get read. This checks the two steps together: not
    // just that `find` returns the right pointer (`find_locates_a_topic_by_tid_in_a_sorted_list`
    // already does that), but that reading through the result gives that SAME topic's own value,
    // never the other topic's, even though both are reached through one erased interface.
    let mut reading = bus::StateTopic::new();
    reading.publish(
        0,
        Reading {
            volts: 12.0,
            ok: true,
        },
    );
    let mut status = bus::StateTopic::new();
    status.publish(0, Status { armed: false });

    let topics: [(bus::Tid, &dyn bus::Topic); 2] = [(0x10, &reading), (0x20, &status)];

    let found = bus::find(&topics, 0x10).unwrap();
    assert_eq!(
        found.read_latest(0),
        Some(message::Value::Float(f64::from(12.0_f32)))
    );

    let found = bus::find(&topics, 0x20).unwrap();
    assert_eq!(found.read_latest(0), Some(message::Value::Bool(false)));

    assert!(bus::find(&topics, 0x30).is_none());
}

#[test]
fn a_generic_consumer_reads_either_kind_of_topic_through_one_interface() {
    // What telemetry or FDIR will do: hold a `&dyn Topic` and call it without knowing which
    // concrete topic type, or message type, is behind it.
    fn volts(topic: &dyn bus::Topic) -> Option<f64> {
        match topic.read_latest(0)? {
            message::Value::Float(volts) => Some(volts),
            _ => None,
        }
    }

    let mut topic = bus::StateTopic::new();
    topic.publish(
        0,
        Reading {
            volts: 4.5,
            ok: true,
        },
    );
    assert_eq!(volts(&topic), Some(4.5));
}
