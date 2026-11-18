//! The core's own commands, events, messages, and tables.
#![forbid(unsafe_code)]
#![no_std]

pub mod core_event;
pub mod event_log;
mod record;
pub mod reset_log;
