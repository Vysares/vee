//! The platform: the OSAL services the core needs, bundled in one trait, so `Core<P: Platform>`
//! has one type parameter however many services it comes to use.
//!
//! A platform is a type with no data, whose only job is to name the types of the services. The
//! services themselves are values the executive makes and hands to `Core::start`. There is one
//! real platform (`linux::LinuxPlatform`) and one for tests (`fake_platform::FakePlatform`).
//!
//! A new service (the file service's thread, say) adds an associated type here, and an
//! implementation to each platform.

use crate::files;
use crate::watchdog;

pub trait Platform {
    /// The file system: the logs, tables and the like.
    type Files: files::Files;
    /// How the supervisor is told that Vee is alive.
    type Watchdog: watchdog::Watchdog;
}
