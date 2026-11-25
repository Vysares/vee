//! Shared types and traits between the core and modules.
#![forbid(unsafe_code)]
#![no_std]

pub mod bus;
pub mod cmd;
pub mod config;
pub mod crc;
pub mod ctx;
pub mod ev;
pub mod event;
pub mod file;
pub mod fixed_str;
pub mod message;
pub mod module;
pub mod queue;
pub mod reset;
pub mod throttle;
