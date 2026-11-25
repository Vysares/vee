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

// ---- Boundaries of the dyn view ----

fn reading(volts: f32) -> Reading {
    Reading { volts, ok: true }
}

fn queued(readers: [bus::Policy; 2]) -> bus::QueuedTopic<Reading, 2, 3> {
    bus::QueuedTopic::new(readers)
}

#[test]
fn pop_encoded_into_a_buffer_exactly_the_messages_size_works() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    topic.publish(reading(2.5)).unwrap();
    let mut buf = [0u8; Reading::WIRE_LEN];
    assert_eq!(topic.pop_encoded(0, &mut buf), Some(Reading::WIRE_LEN));
    assert_eq!(buf, reading(2.5).encode());
}

#[test]
fn pop_encoded_into_a_buffer_one_byte_short_gives_none_and_does_not_lose_the_message() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    topic.publish(reading(2.5)).unwrap();

    let mut short = [0u8; Reading::WIRE_LEN - 1];
    assert_eq!(topic.pop_encoded(0, &mut short), None);
    // The message is still waiting, for a caller that comes back with room for it.
    assert_eq!(topic.len(0), 1);
    let mut buf = [0u8; Reading::WIRE_LEN];
    assert_eq!(topic.pop_encoded(0, &mut buf), Some(Reading::WIRE_LEN));
    assert_eq!(buf, reading(2.5).encode());
    assert!(topic.is_empty(0));
}

#[test]
fn pop_encoded_into_an_empty_buffer_gives_none_and_does_not_lose_the_message() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    topic.publish(reading(1.0)).unwrap();
    assert_eq!(topic.pop_encoded(0, &mut []), None);
    assert_eq!(topic.len(0), 1);
}

#[test]
fn pop_encoded_into_a_bigger_buffer_writes_only_the_messages_bytes() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    topic.publish(reading(2.5)).unwrap();
    let mut buf = [0xAAu8; Reading::WIRE_LEN + 3];
    assert_eq!(topic.pop_encoded(0, &mut buf), Some(Reading::WIRE_LEN));
    assert_eq!(buf[..Reading::WIRE_LEN], reading(2.5).encode());
    assert_eq!(buf[Reading::WIRE_LEN..], [0xAA; 3], "the rest is untouched");
}

#[test]
fn pop_encoded_with_nothing_waiting_gives_none() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    let mut buf = [0u8; 16];
    assert_eq!(topic.pop_encoded(0, &mut buf), None);
}

#[test]
fn pop_encoded_for_the_last_reader_works_and_one_past_it_gives_none() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    topic.publish(reading(1.0)).unwrap();
    let mut buf = [0u8; 16];
    // Readers 0 and 1 exist; 2 does not.
    assert_eq!(topic.pop_encoded(2, &mut buf), None);
    assert_eq!(topic.pop_encoded(usize::MAX, &mut buf), None);
    assert_eq!(topic.len(0), 1);
    assert_eq!(topic.len(1), 1);
    assert!(topic.pop_encoded(1, &mut buf).is_some());
    assert_eq!(topic.len(0), 1, "reader 0's copy is its own");
}

#[test]
fn pop_encoded_takes_messages_oldest_first_and_stops_when_the_queue_is_empty() {
    let mut topic = queued([bus::Policy::Refuse; 2]);
    for volts in [1.0, 2.0, 3.0] {
        topic.publish(reading(volts)).unwrap();
    }
    let mut buf = [0u8; Reading::WIRE_LEN];
    for volts in [1.0, 2.0, 3.0] {
        assert_eq!(topic.pop_encoded(0, &mut buf), Some(Reading::WIRE_LEN));
        assert_eq!(buf, reading(volts).encode());
    }
    assert_eq!(topic.pop_encoded(0, &mut buf), None);
}

#[test]
fn a_state_topic_has_nothing_to_pop() {
    let mut topic = bus::StateTopic::new();
    topic.publish(0, reading(1.0));
    let mut buf = [0u8; 16];
    assert_eq!(topic.pop_encoded(0, &mut buf), None);
    // And popping did nothing to it.
    assert!(topic.latest().is_some());
}

#[test]
fn latest_bytes_into_a_buffer_exactly_the_messages_size_works_and_one_short_does_not() {
    let mut topic = bus::StateTopic::new();
    topic.publish(0, reading(4.0));
    let mut exact = [0u8; Reading::WIRE_LEN];
    assert_eq!(topic.latest_bytes(&mut exact), Some(Reading::WIRE_LEN));
    let mut short = [0u8; Reading::WIRE_LEN - 1];
    assert_eq!(topic.latest_bytes(&mut short), None);
    // A refusal changes nothing.
    assert_eq!(topic.latest_bytes(&mut exact), Some(Reading::WIRE_LEN));
}

#[test]
fn read_latest_with_the_last_field_works_and_one_past_it_gives_none() {
    let mut topic = bus::StateTopic::new();
    topic.publish(0, reading(4.0));
    let last = Reading::LAYOUT.len() - 1;
    assert_eq!(topic.read_latest(last), Some(message::Value::Bool(true)));
    assert_eq!(topic.read_latest(last + 1), None);
    assert_eq!(topic.read_latest(usize::MAX), None);
}

#[test]
fn find_in_an_empty_list_and_at_the_ends_of_a_list() {
    let first = bus::StateTopic::<Status>::new();
    let last = bus::StateTopic::<Status>::new();
    let topics: [(bus::Tid, &dyn bus::Topic); 2] = [(1, &first), (u16::MAX, &last)];
    assert!(bus::find(&[], 1).is_none());
    assert!(bus::find(&topics, 1).is_some());
    assert!(bus::find(&topics, u16::MAX).is_some());
    assert!(bus::find(&topics, 0).is_none(), "below the first");
    assert!(bus::find(&topics, 2).is_none(), "between two");
    assert!(
        bus::find(&topics, u16::MAX - 1).is_none(),
        "just below the last"
    );
}

/// A message of exactly `N` bytes, written by hand (a derive cannot make one this big without a
/// field for every byte), to test the one limit the dyn view has: `MESSAGE_MAX_BYTES`.
#[derive(Debug, Clone, Copy)]
struct Big<const N: usize>;

impl<const N: usize> message::Message for Big<N> {
    const WIRE_LEN: usize = N;
    const LAYOUT: &'static [message::Field] = &[message::Field {
        name: "first",
        offset: 0,
        kind: message::Kind::U8,
    }];

    fn encode_into(&self, out: &mut [u8]) -> Option<usize> {
        out.get_mut(..N)?.fill(7);
        Some(N)
    }
}

#[test]
fn read_latest_works_for_a_message_of_exactly_the_largest_size_and_not_one_byte_more() {
    let mut at_the_limit = bus::StateTopic::<Big<{ message::MESSAGE_MAX_BYTES }>>::new();
    at_the_limit.publish(0, Big);
    assert_eq!(at_the_limit.read_latest(0), Some(message::Value::Int(7)));

    // One byte bigger: `latest_bytes` still works into a buffer that size, but `read_latest` has
    // no buffer that big and says `None` rather than reading half a message.
    let mut over = bus::StateTopic::<Big<{ message::MESSAGE_MAX_BYTES + 1 }>>::new();
    over.publish(0, Big);
    let mut buf = vec![0u8; message::MESSAGE_MAX_BYTES + 1];
    assert_eq!(
        over.latest_bytes(&mut buf),
        Some(message::MESSAGE_MAX_BYTES + 1)
    );
    assert_eq!(over.read_latest(0), None);
}
