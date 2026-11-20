//! The allocation guard: a global allocator that checks the rule "no allocation after startup"
//! (see "General Requirements" in the design). It is a development, SIL and CI check, not a flight
//! protection: a flight build does not declare it.
//!
//! Who calls what:
//! - The deployment's `main` declares it, once per program, and only in builds that want the
//!   check:
//!   `#[global_allocator] static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;`
//!   Rust then calls `GuardAlloc::alloc` for every allocation in the program (`Box`, `Vec`,
//!   `String`, a thread, the standard library's own). Nothing in Vee calls it directly.
//! - The executive's start sequence calls `arm` after startup is over (it is the one place that
//!   does). From then on an allocation aborts the process.
//! - Tests call `allocations_on_this_thread` before and after the code under test, and compare.
//!
//! An allocation after startup that succeeds is a bug to find before flight, and aborting on it
//! in flight would turn that bug into an outage. So there is nothing to switch off in a flight
//! build: it just does not declare the allocator.

use std::alloc;
use std::cell;
use std::io;
use std::io::Write as _;
use std::process;
use std::sync::atomic;
use std::thread;
use std::time;

/// What is written to stderr before aborting. A fixed text: building a message could allocate,
/// and this is the one place that must not.
const ABORT_MESSAGE: &[u8] = b"vee: allocation after startup, aborting\n";

/// How long `arm` waits for other threads to finish starting, before it arms. A new thread
/// allocates in its own startup (std copies its name for the stack overflow handler), and that
/// happens after `spawn` has returned to the thread that made it. So arming straight after
/// starting a thread can catch the thread's own startup, which is not a bug in Vee. A few
/// milliseconds is far longer than any thread needs, and only a build with the guard pays it. This
/// is why the fix is here and not in the code that starts threads: the guard is a development check,
/// and the code it checks should not change to suit it.
const SETTLE: time::Duration = time::Duration::from_millis(100);

/// Whether startup is over. Set once by `arm`, never cleared.
static ARMED: atomic::AtomicBool = atomic::AtomicBool::new(false);

thread_local! {
    /// How many times this thread has allocated since it started. Each thread has its own, so a
    /// test counts only what its own thread does, whatever other threads (other tests, the log
    /// writer) are doing. The `const` form never allocates and never needs cleaning up, which
    /// matters because it is touched inside the allocator.
    static ALLOCATIONS: cell::Cell<usize> = const { cell::Cell::new(0) };
}

/// Marks startup as over. After this, any allocation by any thread aborts the process. It first
/// waits `SETTLE`, so threads started during startup have finished their own startup.
pub fn arm() {
    thread::sleep(SETTLE);
    ARMED.store(true, atomic::Ordering::Release);
}

/// How many allocations the calling thread has made so far. Only grows (it wraps after more than
/// the largest `usize`, which cannot happen in practice). Meaningful only when the program
/// declared `GuardAlloc`.
pub fn allocations_on_this_thread() -> usize {
    ALLOCATIONS.get()
}

/// The allocator to declare with `#[global_allocator]`. It hands the real work to the system
/// allocator and adds the count and the check.
pub struct GuardAlloc;

// `GlobalAlloc` is an `unsafe` trait: the compiler cannot check that an allocator hands out valid
// memory, so the one who writes it promises. We keep the promise by passing every call to
// `System` unchanged, so the memory is exactly what `System` would have given.
//
// Only `alloc` and `dealloc` are written. The trait's provided `alloc_zeroed` and `realloc` call
// `alloc` (and `dealloc`), so they are counted and checked too.
#[allow(
    unsafe_code,
    reason = "implementing GlobalAlloc is unsafe by definition; every call goes to `System`"
)]
// SAFETY: every method forwards its arguments unchanged to `alloc::System`, which meets the
// `GlobalAlloc` contract, and adds only a counter and an abort, neither of which touches the memory.
unsafe impl alloc::GlobalAlloc for GuardAlloc {
    unsafe fn alloc(&self, layout: alloc::Layout) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get().wrapping_add(1));
        if ARMED.load(atomic::Ordering::Acquire) {
            abort_after_startup();
        }
        // SAFETY: the caller of `GuardAlloc::alloc` upholds `alloc`'s contract for `layout`, and
        // we pass the same `layout` on.
        unsafe { alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: alloc::Layout) {
        // SAFETY: the caller upholds `dealloc`'s contract: `ptr` came from `GuardAlloc::alloc`
        // with this `layout`, and that gave it to us from `System::alloc` unchanged.
        unsafe { alloc::System.dealloc(ptr, layout) }
    }
}

/// Writes the fixed message and aborts. `abort` does not unwind and does not run any code that
/// could allocate.
#[allow(
    clippy::let_underscore_must_use,
    reason = "if stderr cannot be written to there is nowhere else to report it, and the process aborts either way"
)]
fn abort_after_startup() -> ! {
    let _ = io::stderr().write_all(ABORT_MESSAGE);
    process::abort()
}

#[cfg(test)]
mod tests {
    use super as std_alloc_guard;

    #[test]
    fn an_allocation_is_counted() {
        let before = std_alloc_guard::allocations_on_this_thread();
        let boxed = Box::new(7_u64);
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(after - before, 1);
        drop(boxed);
    }

    #[test]
    fn freeing_and_plain_arithmetic_are_not_counted() {
        let boxed = Box::new(7_u64);
        let before = std_alloc_guard::allocations_on_this_thread();
        drop(boxed);
        let sum: u64 = (1..=10).sum();
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(sum, 55);
        assert_eq!(after, before);
    }

    #[test]
    fn growing_a_vec_is_counted() {
        let mut bytes = Vec::<u8>::with_capacity(1);
        let before = std_alloc_guard::allocations_on_this_thread();
        bytes.extend_from_slice(&[0; 100]); // does not fit: `realloc`, which goes through `alloc`
        let after = std_alloc_guard::allocations_on_this_thread();
        assert!(after > before);
    }

    #[test]
    fn each_thread_counts_its_own_allocations() {
        let other = std::thread::spawn(|| {
            let before = std_alloc_guard::allocations_on_this_thread();
            let boxed = Box::new(7_u64);
            let after = std_alloc_guard::allocations_on_this_thread();
            drop(boxed);
            after - before
        });
        assert_eq!(other.join().unwrap(), 1);
    }
}
