//! A fixed-size first-in first-out queue, for code that runs on the main thread: the event
//! queue, the bus, the command queues.
//!
//! `Queue<T, N>` holds up to `N` items of any type `T`. `N` is part of the type, so the queue is
//! an array and nothing is allocated. It is not safe to share between threads: queues that cross
//! threads (to the file service, say) are the OSAL's.
//!
//! What to do when the queue is full is the caller's choice, because it differs per queue (see
//! `push`).

/// A ring buffer: the oldest item is at `head`, and `len` items follow it, wrapping round to the
/// start of the array when they pass the end.
pub struct Queue<T, const N: usize> {
    /// `Some` for the `len` slots in use, `None` for the others. (`Option` is how an array can
    /// hold "no item here" for a `T` that has no empty value of its own.)
    slots: [Option<T>; N],
    head: usize,
    len: usize,
}

impl<T, const N: usize> Queue<T, N> {
    pub const fn new() -> Self {
        Queue {
            // `[None; N]` would need `T` to be `Copy`. A `const` block makes a new `None` for
            // each slot, so `T` can be anything.
            slots: [const { None }; N],
            head: 0,
            len: 0,
        }
    }

    /// Adds an item at the back. If the queue is full, nothing is added and the item is given
    /// back in the `Err`, so the caller decides what to do with it:
    /// - drop it (the newest is lost, the oldest are kept): ignore the `Err`
    /// - drop the oldest instead: `pop` one, then `push` again
    /// - report a failure, such as a nak for a busy command queue
    pub fn push(&mut self, item: T) -> Result<(), T> {
        if self.len >= N {
            return Err(item);
        }
        // The slot after the last item, wrapping round. `head` and `len` are both below `N`, so
        // none of these steps can fail. If one ever did, the item is returned and not lost.
        let tail = self.head.checked_add(self.len).and_then(|end| end.checked_rem(N));
        let Some(slot) = tail.and_then(|index| self.slots.get_mut(index)) else {
            return Err(item);
        };
        *slot = Some(item);
        // `len < N` here, so this cannot reach the limit that `saturating_add` guards.
        self.len = self.len.saturating_add(1);
        Ok(())
    }

    /// Takes the oldest item out of the queue, or `None` if it is empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        // `len > 0`, so `N` is not 0 and `head` is a valid index.
        let item = self.slots.get_mut(self.head)?.take();
        // Move on one slot, wrapping round. `N` is not 0, so `checked_rem` does not give `None`.
        self.head = self.head.saturating_add(1).checked_rem(N).unwrap_or(0);
        // `len > 0` here, so this cannot reach the limit that `saturating_sub` guards.
        self.len = self.len.saturating_sub(1);
        item
    }

    /// How many items are waiting.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<T, const N: usize> Default for Queue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super as queue;

    /// Not `Copy` or `Clone`, to show that a queue does not need that of its items.
    #[derive(Debug, PartialEq, Eq)]
    struct Token(u32);

    #[test]
    fn a_new_queue_is_empty() {
        let mut q = queue::Queue::<u32, 4>::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn items_come_out_oldest_first() {
        let mut q = queue::Queue::<u32, 4>::new();
        for item in [10, 20, 30] {
            assert_eq!(q.push(item), Ok(()));
        }
        assert_eq!(q.len(), 3);
        assert!(!q.is_empty());
        for item in [10, 20, 30] {
            assert_eq!(q.pop(), Some(item));
        }
        assert_eq!(q.pop(), None);
        assert!(q.is_empty());
    }

    #[test]
    fn a_full_queue_gives_the_item_back_and_keeps_what_it_has() {
        let mut q = queue::Queue::<u32, 2>::new();
        assert_eq!(q.push(1), Ok(()));
        assert_eq!(q.push(2), Ok(()));
        assert_eq!(q.push(3), Err(3));
        assert_eq!(q.len(), 2);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn a_slot_freed_by_pop_can_be_used_again() {
        let mut q = queue::Queue::<u32, 2>::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.push(3), Ok(()));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
    }

    #[test]
    fn dropping_the_oldest_instead_is_a_pop_then_a_push() {
        let mut q = queue::Queue::<u32, 2>::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        if let Err(item) = q.push(3) {
            q.pop();
            q.push(item).unwrap();
        }
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
    }

    #[test]
    fn order_is_kept_when_the_queue_wraps_round_many_times() {
        let mut q = queue::Queue::<u32, 3>::new();
        let mut next_to_pop = 0;
        for item in 0..20 {
            q.push(item).unwrap();
            // Pop whenever the queue is full, so the ring turns round without ever being full
            // at a push.
            if q.len() == 3 {
                assert_eq!(q.pop(), Some(next_to_pop));
                next_to_pop += 1;
            }
        }
        while let Some(item) = q.pop() {
            assert_eq!(item, next_to_pop);
            next_to_pop += 1;
        }
        assert_eq!(next_to_pop, 20);
    }

    #[test]
    fn a_queue_of_size_one_works() {
        let mut q = queue::Queue::<u32, 1>::new();
        for item in 0..5 {
            assert_eq!(q.push(item), Ok(()));
            assert_eq!(q.push(99), Err(99));
            assert_eq!(q.pop(), Some(item));
        }
    }

    #[test]
    fn a_queue_of_size_zero_holds_nothing_and_does_not_panic() {
        let mut q = queue::Queue::<u32, 0>::new();
        assert_eq!(q.push(1), Err(1));
        assert_eq!(q.pop(), None);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn items_do_not_need_to_be_copy_and_come_back_as_they_went_in() {
        let mut q = queue::Queue::<Token, 2>::new();
        q.push(Token(1)).unwrap();
        q.push(Token(2)).unwrap();
        assert_eq!(q.push(Token(3)), Err(Token(3)));
        assert_eq!(q.pop(), Some(Token(1)));
        assert_eq!(q.pop(), Some(Token(2)));
    }

    #[test]
    fn a_queue_made_with_default_is_empty() {
        let q: queue::Queue<u32, 4> = queue::Queue::default();
        assert!(q.is_empty());
    }
}
