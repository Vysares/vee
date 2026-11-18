//! Thin OS abstraction layer. The only flight crate that may use std and unsafe.

pub mod fake_files;
pub mod files;
pub mod std_files;
