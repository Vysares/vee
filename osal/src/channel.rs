//! A bounded queue between two threads: one thread sends, another receives, and neither is
//! slowed by the other unless it chooses to wait. It wraps `std::sync::mpsc::sync_channel`, which
//! is how the rest of Vee reaches it: nothing above the OSAL names `mpsc`.
//!
//! The queue holds `depth` items, and an item is moved into it and out of it by value, so there is
//! no allocation per item. `channel` allocates once, to make the queue. Sending never waits: a
//! full queue gives the item back (`try_send`), so the sender decides what to do (the main thread
//! never waits on a file). Receiving can wait (`recv`), which is for a worker thread that has
//! nothing else to do, or not (`try_recv`), which is for the main thread, once a tick.
//!
//! Who calls what: the file service makes two of these, one for requests (the main thread sends and
//! the worker receives) and one for responses (the other way round).

use std::sync::mpsc;
use std::time;

/// The sending end. There is one, and it is not `Clone`: a queue with one sender and one receiver
/// is what Vee needs, and it keeps who sends easy to see.
pub struct Sender<T> {
    inner: mpsc::SyncSender<T>,
}

/// The receiving end.
pub struct Receiver<T> {
    inner: mpsc::Receiver<T>,
}

/// A queue that holds `depth` items. A `depth` of 0 is made 1: for `mpsc`, 0 means "a sender waits
/// for a receiver to be there", which is a different thing and not what a queue is. (The file
/// service's depth is a build-time setting with a minimum of 1, so this does not happen.)
pub fn channel<T>(depth: usize) -> (Sender<T>, Receiver<T>) {
    let (inner_sender, inner_receiver) = mpsc::sync_channel(depth.max(1));
    (
        Sender {
            inner: inner_sender,
        },
        Receiver {
            inner: inner_receiver,
        },
    )
}

impl<T> Sender<T> {
    /// Puts `item` in the queue and returns at once. If the queue is full, or the receiver has
    /// gone (its thread ended), the item comes back in the `Err`, as `Queue::push` does, and the
    /// sender decides what to do with it. A receiver that has gone is not told apart from a full
    /// queue: the thread it was on aborted the process, in a flight build.
    pub fn try_send(&self, item: T) -> Result<(), T> {
        self.inner.try_send(item).map_err(|error| match error {
            mpsc::TrySendError::Full(item) | mpsc::TrySendError::Disconnected(item) => item,
        })
    }
}

impl<T> Receiver<T> {
    /// The oldest item, or `None` if the queue is empty (or the sender has gone and the queue is
    /// empty). Returns at once.
    pub fn try_recv(&self) -> Option<T> {
        self.inner.try_recv().ok()
    }

    /// The oldest item. If the queue is empty, WAITS until there is one. Returns `None` once the
    /// sender has gone and the queue is empty, which is how a worker learns to stop. Only a thread
    /// that has nothing else to do calls this.
    pub fn recv(&self) -> Option<T> {
        self.inner.recv().ok()
    }

    /// The oldest item. If the queue is empty, WAITS until there is one, but for at most
    /// `timeout_us` microseconds, and then gives `None`. It also gives `None` at once if the
    /// sender has gone and the queue is empty. This is for the one place that has something
    /// to wait for and must not wait for ever: shutdown, waiting for a worker to finish.
    pub fn recv_timeout(&self, timeout_us: u64) -> Option<T> {
        self.inner
            .recv_timeout(time::Duration::from_micros(timeout_us))
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super as channel;
    use crate::linux::std_alloc_guard;
    use std::thread;
    use std::time;

    #[test]
    fn items_come_out_in_the_order_they_went_in() {
        let (sender, receiver) = channel::channel(4);
        for item in 1..=3 {
            sender.try_send(item).unwrap();
        }
        assert_eq!(receiver.try_recv(), Some(1));
        assert_eq!(receiver.try_recv(), Some(2));
        assert_eq!(receiver.try_recv(), Some(3));
        assert_eq!(receiver.try_recv(), None);
    }

    #[test]
    fn a_full_queue_gives_the_item_back_and_keeps_the_others() {
        let (sender, receiver) = channel::channel(2);
        sender.try_send("a").unwrap();
        sender.try_send("b").unwrap();
        assert_eq!(sender.try_send("c"), Err("c")); // full: the item is handed back
        assert_eq!(receiver.try_recv(), Some("a"));
        sender.try_send("c").unwrap(); // room again
        assert_eq!(receiver.try_recv(), Some("b"));
        assert_eq!(receiver.try_recv(), Some("c"));
    }

    #[test]
    fn a_depth_of_zero_is_a_depth_of_one() {
        let (sender, receiver) = channel::channel(0);
        sender.try_send(1).unwrap();
        assert_eq!(sender.try_send(2), Err(2));
        assert_eq!(receiver.try_recv(), Some(1));
    }

    #[test]
    fn a_send_after_the_receiver_is_gone_gives_the_item_back() {
        let (sender, receiver) = channel::channel(2);
        drop(receiver);
        assert_eq!(sender.try_send(5), Err(5));
    }

    #[test]
    fn recv_waits_for_an_item_from_another_thread() {
        let (sender, receiver) = channel::channel(1);
        let started = time::Instant::now();
        let sending = thread::spawn(move || {
            thread::sleep(time::Duration::from_millis(30));
            sender.try_send(9).unwrap();
        });
        assert_eq!(receiver.recv(), Some(9));
        assert!(started.elapsed() >= time::Duration::from_millis(30));
        sending.join().unwrap();
    }

    #[test]
    fn recv_timeout_returns_a_queued_item_at_once() {
        let (sender, receiver) = channel::channel(1);
        sender.try_send(4).unwrap();
        let started = time::Instant::now();
        assert_eq!(receiver.recv_timeout(10_000_000), Some(4));
        assert!(started.elapsed() < time::Duration::from_secs(5));
    }

    #[test]
    fn recv_timeout_gives_none_after_the_timeout_when_nothing_comes() {
        let (_sender, receiver) = channel::channel::<u8>(1);
        let started = time::Instant::now();
        assert_eq!(receiver.recv_timeout(20_000), None);
        assert!(started.elapsed() >= time::Duration::from_millis(20));
    }

    #[test]
    fn recv_timeout_returns_an_item_that_comes_before_the_timeout() {
        let (sender, receiver) = channel::channel(1);
        let sending = thread::spawn(move || {
            thread::sleep(time::Duration::from_millis(20));
            sender.try_send(8).unwrap();
        });
        assert_eq!(receiver.recv_timeout(10_000_000), Some(8));
        sending.join().unwrap();
    }

    #[test]
    fn recv_timeout_gives_none_at_once_when_the_sender_is_gone_and_the_queue_is_empty() {
        let (sender, receiver) = channel::channel::<u8>(1);
        drop(sender);
        let started = time::Instant::now();
        assert_eq!(receiver.recv_timeout(10_000_000), None);
        assert!(started.elapsed() < time::Duration::from_secs(5));
    }

    #[test]
    fn recv_returns_none_when_the_sender_is_gone_and_the_queue_is_empty() {
        let (sender, receiver) = channel::channel(2);
        sender.try_send(1).unwrap();
        drop(sender);
        assert_eq!(receiver.recv(), Some(1)); // what was queued is still delivered
        assert_eq!(receiver.recv(), None);
        assert_eq!(receiver.try_recv(), None);
    }

    #[test]
    fn sending_and_receiving_a_large_item_does_not_allocate() {
        // About the size of a file request.
        let (sender, receiver) = channel::channel::<[u8; 4096]>(2);
        let item = [7_u8; 4096];
        let before = std_alloc_guard::allocations_on_this_thread();
        sender.try_send(item).unwrap();
        let received = receiver.try_recv();
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(after, before);
        assert_eq!(received, Some(item));
    }
}
