//! Thin OS abstraction layer. The only flight crate that may use std and unsafe.
//!
//! Each service has an interface file with the traits and plain types, which uses only `core`
//! (`clock`, `files`, `syslog`), and one file for each implementation, named for its platform
//! (`std_clock`, `std_files`, `std_syslog`) or for a test double (`fake_clock`, `fake_files`). The
//! rest of Vee uses the interface. A new platform adds files and changes nothing else.
// Unsafe is denied here too (the workspace already says so). A block that needs it must carry
// `#[allow(unsafe_code, reason = "...")]` and a `// SAFETY:` comment.
#![deny(unsafe_code)]

pub mod clock;
pub mod fake_clock;
pub mod fake_files;
pub mod files;
pub mod std_clock;
pub mod std_files;
pub mod std_syslog;
pub mod syslog;
