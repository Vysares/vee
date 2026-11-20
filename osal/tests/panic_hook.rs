//! The panic hook, end to end. Installing a hook is global and a test that expects a panic would
//! leave `PANICKING` set, so this file is its own program, and the one test runs the real work in a
//! child process (the same program run again) and checks what the child printed.
//!
//! The child installs the hook, panics on a named thread, and reports what it saw on stdout. The
//! hook writes its line to the child's stderr, which the parent reads.

// The child's helper function is not a `#[test]`, so the exemptions in `clippy.toml` do not cover it.
// It is test code: it prints what it saw for the parent to read, and may unwrap and subtract.
#![allow(
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "test code: the child panics on purpose, reports on stdout, and a failure should panic"
)]

use std::process;
use vee_osal::linux::std_alloc_guard;
use vee_osal::linux::std_clock;
use vee_osal::linux::std_panic;

// So the child can count allocations around `install`.
#[global_allocator]
static GUARD: std_alloc_guard::GuardAlloc = std_alloc_guard::GuardAlloc;

/// The parent sets this variable for the child, which is how the same test knows which one it is.
/// Its value is the number the child panics with, so the message has a value known only at run
/// time (a `panic!` with a value in it formats the message, which is what we want to see).
const CHILD_MARKER: &str = "VEE_PANIC_HOOK_CHILD";
const PANIC_NUMBER: &str = "7";
const TEST_NAME: &str = "a_panic_on_a_thread_writes_one_report_line_and_sets_the_flag";

/// What the child does. It prints `key=value` lines for the parent to check.
fn child() {
    println!("panicking_before={}", std_panic::is_panicking());

    let before = std_alloc_guard::allocations_on_this_thread();
    let first = std_panic::install(std_clock::StdClock::new());
    let after = std_alloc_guard::allocations_on_this_thread();
    println!("install_ok={}", first.is_ok());
    println!("install_allocations={}", after - before);
    println!(
        "second_install={:?}",
        std_panic::install(std_clock::StdClock::new())
    );

    let number: u32 = std::env::var(CHILD_MARKER).unwrap().parse().unwrap();
    let worker = std::thread::Builder::new()
        .name("worker".into())
        .spawn(move || panic!("boom {number}"))
        .unwrap();
    println!("worker_panicked={}", worker.join().is_err());
    println!("panicking_after={}", std_panic::is_panicking());
}

#[test]
fn a_panic_on_a_thread_writes_one_report_line_and_sets_the_flag() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        child();
        return;
    }

    let output = process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MARKER, PANIC_NUMBER)
        .output()
        .unwrap();
    assert!(output.status.success(), "the child failed: {output:?}");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("panicking_before=false"), "{stdout}");
    assert!(stdout.contains("install_ok=true"), "{stdout}");
    assert!(stdout.contains("install_allocations=0"), "{stdout}");
    assert!(
        stdout.contains("second_install=Err(AlreadyInstalled)"),
        "{stdout}"
    );
    assert!(stdout.contains("worker_panicked=true"), "{stdout}");
    assert!(stdout.contains("panicking_after=true"), "{stdout}");

    // One report line, with the thread, the location in this file, and the formatted message.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let report: Vec<&str> = stderr.lines().filter(|l| l.contains(" panic: ")).collect();
    assert_eq!(report.len(), 1, "stderr was: {stderr}");
    assert!(report[0].contains(" ERROR panic: thread 'worker' panicked at "));
    assert!(report[0].contains("panic_hook.rs:"));
    assert!(report[0].ends_with(": boom 7"), "{}", report[0]);
}
