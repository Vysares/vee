//! Tests of the event service. They are here, not in `ev.rs`, so they can use
//! `#[derive(event::Code)]` (inside `core-api` itself the derive's `::vee_core_api` paths do not
//! resolve).

use core::fmt;
use vee_core_api::ev;
use vee_core_api::event;
use vee_core_api::fixed_str;

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum SampleEv {
    #[severity(Info)]
    Started,
    #[severity(Warn)]
    Slow,
}

const SOURCE: u8 = 3;
const DEPTH: usize = ev::EV_QUEUE_DEPTH;

fn send(queue: &mut ev::Ev, time: u64, text: fmt::Arguments<'_>) {
    queue.send(SOURCE, time, SampleEv::Started, text);
}

#[test]
fn the_default_depth_is_1024() {
    assert_eq!(ev::EV_QUEUE_DEPTH, 1024);
}

#[test]
fn a_new_queue_is_empty() {
    let mut queue = ev::Ev::new();
    assert!(queue.is_empty());
    assert_eq!(queue.len(), 0);
    assert_eq!(queue.pop(), None);
    assert_eq!(queue.take_dropped(), 0);
}

#[test]
fn a_sent_event_has_the_source_time_code_severity_and_text() {
    let mut queue = ev::Ev::new();
    queue.send(SOURCE, 1_500, SampleEv::Slow, format_args!("hello"));
    let event = queue.pop().unwrap();
    assert_eq!(event.source, SOURCE);
    assert_eq!(event.monotonic_us, 1_500);
    assert_eq!(event.code, 1);
    assert_eq!(event.severity, event::Severity::Warn);
    assert_eq!(event.text.as_bytes(), b"hello");
}

#[test]
fn values_in_the_text_are_formatted_in() {
    let mut queue = ev::Ev::new();
    let (name, err) = ("log", 7);
    send(&mut queue, 0, format_args!("open {name} failed ({err})"));
    assert_eq!(queue.pop().unwrap().text.as_bytes(), b"open log failed (7)");
}

#[test]
fn text_that_is_too_long_is_cut_off_and_marked_but_the_event_is_still_sent() {
    let mut queue = ev::Ev::new();
    let long = [b'x'; event::EV_TEXT_MAX_BYTES + 10];
    let long = core::str::from_utf8(&long).unwrap();
    send(&mut queue, 0, format_args!("{long}"));
    let event = queue.pop().unwrap();
    assert_eq!(event.text.as_bytes().len(), event::EV_TEXT_MAX_BYTES);
    assert_eq!(
        event.text.as_bytes().last(),
        Some(&fixed_str::TRUNCATED_MARK)
    );
}

/// A value whose `Display` fails part way through.
struct Fails;

impl fmt::Display for Fails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ab")?;
        Err(fmt::Error)
    }
}

#[test]
fn a_value_that_fails_to_print_still_gives_an_event_with_the_text_so_far() {
    let mut queue = ev::Ev::new();
    send(&mut queue, 0, format_args!("got {}", Fails));
    assert_eq!(queue.pop().unwrap().text.as_bytes(), b"got ab");
}

#[test]
fn events_come_out_oldest_first() {
    let mut queue = ev::Ev::new();
    for time in [10, 20, 30] {
        send(&mut queue, time, format_args!("e"));
    }
    assert_eq!(queue.len(), 3);
    for time in [10, 20, 30] {
        assert_eq!(queue.pop().unwrap().monotonic_us, time);
    }
    assert_eq!(queue.pop(), None);
}

#[test]
fn a_full_queue_drops_the_new_event_keeps_the_old_ones_and_counts() {
    let mut queue = ev::Ev::new();
    // Two more than fit. The first `DEPTH` are kept and the last two are dropped.
    for time in 0..DEPTH as u64 + 2 {
        send(&mut queue, time, format_args!("e"));
    }
    assert_eq!(queue.len(), DEPTH);
    for time in 0..DEPTH as u64 {
        assert_eq!(queue.pop().unwrap().monotonic_us, time);
    }
    assert_eq!(queue.pop(), None);
    assert_eq!(queue.take_dropped(), 2);
}

#[test]
fn taking_the_dropped_count_starts_it_again_from_zero() {
    let mut queue = ev::Ev::new();
    for time in 0..DEPTH as u64 + 3 {
        send(&mut queue, time, format_args!("e"));
    }
    assert_eq!(queue.take_dropped(), 3);
    assert_eq!(queue.take_dropped(), 0);
}

#[test]
fn a_slot_freed_by_pop_can_be_used_again() {
    let mut queue = ev::Ev::new();
    for time in 0..DEPTH as u64 {
        send(&mut queue, time, format_args!("e"));
    }
    queue.pop().unwrap();
    send(&mut queue, 100, format_args!("e"));
    assert_eq!(queue.take_dropped(), 0);
    assert_eq!(queue.len(), DEPTH);
}
