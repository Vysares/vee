//! The software bus (see `docs/bus.md`). So far: the state topic.
//!
//! A STATE topic holds the latest value of something (a sensor reading, a mode). It is one slot,
//! and every module that reads it looks at that same slot: nothing is copied per reader. It is
//! lossy on purpose: two publishes in one tick leave only the last value, and `seq` shows how
//! many publishes there were.
//!
//! A module never holds a topic. The executive owns the topics and, for each call into a module,
//! makes the handles that module declared: a `StateWriter` for a topic it writes (it can only
//! publish), a `StateReader` for one it reads (it can only look). The compiler then keeps the
//! two apart.
//!
//! A QUEUED topic is different: there is one queue PER READER (`QueuedTopic`, below), so a slow
//! reader never loses what a fast one already saw, and each reader's queue has its own policy for
//! what happens when it is full.
//!
//! Not here yet: where a module gets its handles (`Module::In` and `Out`, `ctx.inputs` and
//! `ctx.out`; done for state topics, not yet for queued ones), APIDs and the layout of a message.

use crate::queue;

/// The latest value of a state topic, with when and which publish it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample<T> {
    pub value: T,
    /// How many times the topic has been published, counting this one. The first publish is 1.
    /// A reader remembers the last `seq` it saw, and a different one means a new value (or
    /// several: the difference says how many, if it has not wrapped).
    pub seq: u32,
    /// The monotonic time, in microseconds, of the step that published it.
    pub updated_at: u64,
}

/// One state topic: the latest `Sample`, or nothing if it was never published. (There is no
/// default value, because a made-up reading looks the same as a real one.)
pub struct StateTopic<T> {
    latest: Option<Sample<T>>,
}

impl<T> StateTopic<T> {
    pub const fn new() -> Self {
        StateTopic { latest: None }
    }

    /// Replaces the value. `now` is the time of the publishing step, and `seq` goes up by one.
    ///
    /// `seq` wraps round after `u32::MAX` rather than stopping: a reader compares it with the
    /// last one it saw, and a counter that stuck at the top would look like "no new value" for
    /// ever. (At 100 publishes a second that takes 497 days.)
    pub fn publish(&mut self, now: u64, value: T) {
        let seq = match &self.latest {
            Some(previous) => previous.seq.wrapping_add(1),
            None => 1,
        };
        self.latest = Some(Sample {
            value,
            seq,
            updated_at: now,
        });
    }

    /// The latest sample, or `None` if the topic was never published. The reference lasts only as
    /// long as the borrow of the topic, so a module that wants the value later copies it out.
    pub fn latest(&self) -> Option<&Sample<T>> {
        self.latest.as_ref()
    }

    /// The handle for the topic's one writer, for one call into that module. `now` is the time
    /// the executive read for that call, so every publish in the call has the same time (as
    /// `Ctx::monotonic` does). It borrows the topic mutably, so while it lives nothing else can
    /// read or write the topic: that is checked by the compiler, not by a rule.
    pub fn writer(&mut self, now: u64) -> StateWriter<'_, T> {
        StateWriter { topic: self, now }
    }

    /// A handle for a reader, for one call into that module. It borrows the topic immutably, so
    /// any number of readers can exist at once, and none while a writer does.
    pub fn reader(&self) -> StateReader<'_, T> {
        StateReader { topic: self }
    }
}

impl<T> Default for StateTopic<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// What a module that WRITES a state topic holds (in its `ctx.out`): it can publish and nothing
/// else. It cannot read the topic back, and it does not choose the time.
///
/// `'a` is how long the handle borrows the topic. The executive makes one for each call into the
/// module and drops it when the call returns, so a module cannot keep it.
pub struct StateWriter<'a, T> {
    topic: &'a mut StateTopic<T>,
    now: u64,
}

impl<T> StateWriter<'_, T> {
    /// Publishes a new value: it replaces the old one, and `seq` goes up by one.
    pub fn publish(&mut self, value: T) {
        self.topic.publish(self.now, value);
    }
}

/// What a module that READS a state topic holds (in its `ctx.inputs`): it can look at the latest
/// sample and nothing else.
pub struct StateReader<'a, T> {
    topic: &'a StateTopic<T>,
}

impl<'a, T> StateReader<'a, T> {
    /// The latest sample, or `None` if the topic was never published. The reference is good for
    /// `'a`, the borrow of the topic, and not only for as long as this handle is borrowed, so it
    /// can be held for the rest of the call. It cannot outlive the call: a module that wants the
    /// value later copies it out.
    pub fn latest(&self) -> Option<&'a Sample<T>> {
        self.topic.latest()
    }
}

/// How a reader's queue behaves when it is full (see `docs/bus.md`, "Queued topics"). One reader
/// is often flow controlled (a link that must not lose data silently), while another is lossy
/// (telemetry), so the policy is chosen per reader, not per topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// `publish` reports `Err(Full)` when this reader's queue has no room, so the writer notices.
    /// A flow controlled writer checks `space()` first and should rarely see this.
    Refuse,
    /// A full queue drops the new message instead, and counts it (`Queue::take_refused`).
    /// Turning that into an event is the caller's job, once per tick, not here.
    Drop,
}

/// `publish` could not deliver to at least one `Refuse` reader, because its queue was full. The
/// value published is `Copy`, so the writer still has it and can try again later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// A queued topic: one writer, `N` readers, one queue per reader, all the same depth `D`. Unlike a
/// state topic, every reader gets its OWN COPY of every message, so nothing is overwritten before
/// a slow reader sees it (this is also why `T` must be `Copy`: one message becomes `N` copies).
pub struct QueuedTopic<T, const N: usize, const D: usize> {
    queues: [queue::Queue<T, D>; N],
    policies: [Policy; N],
}

impl<T: Copy, const N: usize, const D: usize> QueuedTopic<T, N, D> {
    /// `policies[i]` governs reader `i`'s queue.
    pub const fn new(policies: [Policy; N]) -> Self {
        QueuedTopic {
            queues: [const { queue::Queue::new() }; N],
            policies,
        }
    }

    /// Delivers `value` to every reader's queue that has room: one queue being full never stops
    /// delivery to the others. Reports `Err(Full)` if any `Refuse` queue was full; a full `Drop`
    /// queue just drops the message (and counts it) without affecting the result.
    pub fn publish(&mut self, value: T) -> Result<(), Full> {
        let mut refused = false;
        for (queue, policy) in self.queues.iter_mut().zip(self.policies.iter()) {
            if queue.push(value).is_err() && *policy == Policy::Refuse {
                refused = true;
            }
        }
        if refused { Err(Full) } else { Ok(()) }
    }

    /// The next message waiting for one reader, oldest first, or `None` if there is none (or
    /// `reader` is out of range).
    pub fn pop(&mut self, reader: usize) -> Option<T> {
        self.queues.get_mut(reader)?.pop()
    }

    /// How many messages are still waiting for one reader (0 if `reader` is out of range).
    pub fn len(&self, reader: usize) -> usize {
        self.queues.get(reader).map_or(0, |queue| queue.len())
    }

    pub fn is_empty(&self, reader: usize) -> bool {
        self.len(reader) == 0
    }

    /// How many messages were dropped for one reader (a `Drop` queue that was full) since this
    /// was last called, and the count starts again from 0 (0 if `reader` is out of range). The
    /// caller turns this into an event once per tick, the same way `Ev`'s queue does.
    pub fn take_refused(&mut self, reader: usize) -> u32 {
        self.queues
            .get_mut(reader)
            .map_or(0, |queue| queue.take_refused())
    }

    /// The smallest number of free slots among all readers: how many more messages `publish` can
    /// deliver everywhere before some reader's queue is full. `D` (no bottleneck) if there are no
    /// readers.
    pub fn space(&self) -> usize {
        self.queues
            .iter()
            .map(|queue| D.saturating_sub(queue.len()))
            .min()
            .unwrap_or(D)
    }
}

#[cfg(test)]
mod tests {
    use super as bus;

    #[test]
    fn a_new_topic_has_no_value() {
        let topic = bus::StateTopic::<u32>::new();
        assert_eq!(topic.latest(), None);
    }

    #[test]
    fn the_first_publish_is_seq_1_with_its_time() {
        let mut topic = bus::StateTopic::new();
        topic.publish(5_000, 42_u32);
        assert_eq!(
            topic.latest(),
            Some(&bus::Sample {
                value: 42,
                seq: 1,
                updated_at: 5_000
            })
        );
    }

    #[test]
    fn two_publishes_leave_the_last_and_seq_counts_both() {
        let mut topic = bus::StateTopic::new();
        topic.publish(100, 1_u32);
        topic.publish(100, 2);
        assert_eq!(
            topic.latest(),
            Some(&bus::Sample {
                value: 2,
                seq: 2,
                updated_at: 100
            })
        );
    }

    #[test]
    fn reading_does_not_change_the_topic() {
        let mut topic = bus::StateTopic::new();
        topic.publish(100, 7_u32);
        let first = topic.latest().copied();
        let second = topic.latest().copied();
        assert_eq!(first, second);
        assert_eq!(first.map(|sample| sample.seq), Some(1));
    }

    #[test]
    fn seq_wraps_instead_of_stopping() {
        let mut topic = bus::StateTopic::new();
        topic.latest = Some(bus::Sample {
            value: 0_u32,
            seq: u32::MAX,
            updated_at: 0,
        });
        topic.publish(1, 9);
        // It went from the largest value to 0: different from the one before, which is all a
        // reader checks.
        assert_eq!(topic.latest().map(|sample| sample.seq), Some(0));
        assert_eq!(topic.latest().map(|sample| sample.value), Some(9));
    }

    /// Not `Copy` or `Clone`: a message will be `Copy` once the derive exists, but the topic itself
    /// does not need it.
    #[derive(Debug, PartialEq, Eq)]
    struct Token(u32);

    #[test]
    fn a_writer_publishes_with_the_time_it_was_made_with() {
        let mut topic = bus::StateTopic::new();
        let mut writer = topic.writer(2_000);
        writer.publish(1_u32);
        writer.publish(2);
        // Both publishes in the call have the call's time, and seq counts both.
        assert_eq!(
            topic.latest(),
            Some(&bus::Sample {
                value: 2,
                seq: 2,
                updated_at: 2_000
            })
        );
    }

    #[test]
    fn a_reader_sees_what_a_writer_published_and_nothing_before() {
        let mut topic = bus::StateTopic::new();
        assert_eq!(topic.reader().latest(), None);
        topic.writer(10).publish(5_u32);
        let sample = topic.reader().latest().copied();
        assert_eq!(
            sample.map(|s| (s.value, s.seq, s.updated_at)),
            Some((5, 1, 10))
        );
    }

    #[test]
    fn two_readers_look_at_the_same_slot() {
        let mut topic = bus::StateTopic::new();
        topic.writer(10).publish(5_u32);
        let first = topic.reader();
        let second = topic.reader();
        // The same sample, at the same address: nothing was copied per reader.
        assert!(core::ptr::eq(
            first.latest().unwrap(),
            second.latest().unwrap()
        ));
    }

    #[test]
    fn a_later_call_gets_a_new_time() {
        let mut topic = bus::StateTopic::new();
        topic.writer(10).publish(1_u32);
        topic.writer(20).publish(2);
        let sample = topic.reader().latest().copied();
        assert_eq!(
            sample.map(|s| (s.value, s.seq, s.updated_at)),
            Some((2, 2, 20))
        );
    }

    #[test]
    fn a_value_that_is_not_copy_can_be_published_and_read() {
        let mut topic = bus::StateTopic::new();
        topic.publish(1, Token(7));
        assert_eq!(topic.latest().map(|sample| &sample.value), Some(&Token(7)));
    }

    #[test]
    fn a_new_queued_topic_has_nothing_waiting_for_any_reader() {
        let topic = bus::QueuedTopic::<u32, 2, 4>::new([bus::Policy::Drop, bus::Policy::Drop]);
        assert_eq!(topic.len(0), 0);
        assert_eq!(topic.len(1), 0);
        assert!(topic.is_empty(0));
    }

    #[test]
    fn publish_delivers_a_copy_to_every_reader() {
        let mut topic = bus::QueuedTopic::<u32, 2, 4>::new([bus::Policy::Drop, bus::Policy::Drop]);
        topic.publish(42).unwrap();
        assert_eq!(topic.len(0), 1);
        assert_eq!(topic.len(1), 1);
        assert_eq!(topic.pop(0), Some(42));
        assert_eq!(topic.pop(1), Some(42));
    }

    #[test]
    fn readers_are_independent_popping_one_does_not_affect_another() {
        let mut topic = bus::QueuedTopic::<u32, 2, 4>::new([bus::Policy::Drop, bus::Policy::Drop]);
        topic.publish(1).unwrap();
        topic.publish(2).unwrap();
        assert_eq!(topic.pop(0), Some(1));
        assert_eq!(topic.pop(0), Some(2));
        assert_eq!(topic.pop(0), None);
        // Reader 1 never popped: both messages are still waiting for it, oldest first.
        assert_eq!(topic.pop(1), Some(1));
        assert_eq!(topic.pop(1), Some(2));
    }

    #[test]
    fn a_full_refuse_queue_reports_err_full_and_keeps_what_it_had() {
        let mut topic = bus::QueuedTopic::<u32, 1, 1>::new([bus::Policy::Refuse]);
        topic.publish(1).unwrap();
        assert_eq!(topic.publish(2), Err(bus::Full));
        // The new value was refused; the old one is still there, not overwritten.
        assert_eq!(topic.pop(0), Some(1));
        assert_eq!(topic.pop(0), None);
    }

    #[test]
    fn a_full_drop_queue_drops_the_new_message_but_publish_still_reports_ok() {
        let mut topic = bus::QueuedTopic::<u32, 1, 1>::new([bus::Policy::Drop]);
        topic.publish(1).unwrap();
        assert_eq!(topic.publish(2), Ok(()));
        assert_eq!(topic.pop(0), Some(1));
        assert_eq!(topic.pop(0), None); // 2 was dropped, not queued
        assert_eq!(topic.take_refused(0), 1);
        assert_eq!(topic.take_refused(0), 0); // the count starts again
    }

    #[test]
    fn one_full_queue_does_not_stop_delivery_to_the_others() {
        let mut topic =
            bus::QueuedTopic::<u32, 2, 1>::new([bus::Policy::Refuse, bus::Policy::Drop]);
        topic.publish(1).unwrap();
        assert_eq!(topic.pop(1), Some(1)); // reader 1 catches up, freeing its queue
        // Reader 0 never popped, so its queue (depth 1) is still full.
        assert_eq!(topic.publish(2), Err(bus::Full));
        assert_eq!(topic.pop(0), Some(1)); // reader 0 never saw 2
        assert_eq!(topic.pop(1), Some(2)); // reader 1 had room and got it
    }

    #[test]
    fn space_is_the_smallest_free_count_among_readers() {
        let mut topic = bus::QueuedTopic::<u32, 2, 3>::new([bus::Policy::Drop, bus::Policy::Drop]);
        assert_eq!(topic.space(), 3);
        topic.publish(1).unwrap();
        assert_eq!(topic.pop(0), Some(1)); // reader 0 keeps up
        // Reader 1 never pops: it is the bottleneck.
        assert_eq!(topic.space(), 2);
    }

    #[test]
    fn a_topic_with_no_readers_has_no_bottleneck() {
        let topic = bus::QueuedTopic::<u32, 0, 5>::new([]);
        assert_eq!(topic.space(), 5);
    }
}
