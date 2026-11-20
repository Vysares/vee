//! Thin OS abstraction layer. The only flight crate that may use std and unsafe.
//!
//! Each service has an interface file with the traits and plain types, which uses only `core`
//! (`clock`, `files`, `syslog`, `panic_report`, `watchdog`, `platform`), and one file for each
//! implementation, named for its platform (`std_clock`, `std_files`, `std_syslog`, `std_watchdog`,
//! all in `linux/`) or for a test double (`fake_clock`, `fake_files`, `fake_watchdog`,
//! `fake_platform`). The rest of Vee uses the interface. A new platform adds a directory like
//! `linux/` and changes nothing else.
// Unsafe is denied here too (the workspace already says so). A block that needs it must carry
// `#[allow(unsafe_code, reason = "...")]` and a `// SAFETY:` comment.
#![deny(unsafe_code)]

pub mod clock;
pub mod fake_clock;
pub mod fake_files;
pub mod fake_platform;
pub mod fake_watchdog;
pub mod files;
pub mod linux;
pub mod panic_report;
pub mod platform;
pub mod syslog;
pub mod watchdog;

// The osal's own tests count allocations (see `linux::std_alloc_guard`), so the test program uses the
// guard allocator. It is never armed here: arming would abort the other tests.
#[cfg(test)]
#[global_allocator]
static GUARD: linux::std_alloc_guard::GuardAlloc = linux::std_alloc_guard::GuardAlloc;
