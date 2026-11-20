//! The Linux implementations of the OSAL's interfaces, on the standard library (`std`). Each file
//! implements an interface file one level up (`std_clock` implements `clock`, and so on), except
//! `std_alloc_guard`, which is a development check with no interface, and `std_panic`, the panic
//! hook, which uses the `core`-only `panic_report`. `LinuxPlatform`, below, bundles the
//! implementations the core uses (see `platform`). A port to another platform is a sibling
//! directory with the same shape.

pub mod std_alloc_guard;
pub mod std_clock;
pub mod std_files;
pub mod std_panic;
pub mod std_syslog;
pub mod std_watchdog;

use crate::platform;

/// The real platform on Linux: the real file system and systemd's watchdog. Never made, only
/// named: `Core::<LinuxPlatform>::start(..)` (see `platform`).
pub struct LinuxPlatform;

impl platform::Platform for LinuxPlatform {
    type Files = std_files::StdFiles;
    type Watchdog = std_watchdog::StdWatchdog;
}
