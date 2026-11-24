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
//! Not here yet: where a module gets its handles (`Module::In` and `Out`, `ctx.inputs` and
//! `ctx.out`), queued topics, APIDs and the layout of a message.

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
}
