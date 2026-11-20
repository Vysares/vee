//! The armed guard aborts on an allocation. Aborting kills the whole process, so the test runs
//! its own test program again as a child process, arms the guard there, allocates, and checks
//! how the child ended. This file is its own program (every file in `tests/` is), so declaring
//! the guard as its allocator affects nothing else.

use std::os::unix::process::ExitStatusExt as _;
use std::process;
use vee_osal::linux::std_alloc_guard;

#[global_allocator]
static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;

/// The parent sets this variable for the child, which is how the same test knows which one it is.
const CHILD_MARKER: &str = "VEE_ALLOC_GUARD_CHILD";
const TEST_NAME: &str = "an_allocation_after_arm_aborts_with_a_fixed_message";
/// `abort` ends the process with this signal on Linux.
const SIGABRT: i32 = 6;

#[test]
fn an_allocation_after_arm_aborts_with_a_fixed_message() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        std_alloc_guard::arm();
        let boxed = Box::new(7_u64); // aborts here
        std::hint::black_box(boxed);
        return; // reaching this line means the guard did not abort: the parent will see exit 0
    }

    let output = process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MARKER, "1")
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(SIGABRT));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("vee: allocation after startup, aborting"),
        "stderr was: {stderr}"
    );
}

#[test]
fn an_allocation_before_arm_is_fine() {
    // Not armed in this test (the other test arms only in its child process).
    let boxed = Box::new(7_u64);
    assert_eq!(*boxed, 7);
}
