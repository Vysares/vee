//! Thin OS abstraction layer. The only flight crate that may use std and unsafe.
// Unsafe is denied here too (the workspace already says so). A block that needs it must carry
// `#[allow(unsafe_code, reason = "...")]` and a `// SAFETY:` comment.
#![deny(unsafe_code)]

pub mod clock;
pub mod fake_clock;
pub mod fake_files;
pub mod files;
pub mod std_clock;
pub mod std_files;
