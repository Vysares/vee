//! The OSAL's watchdog API: how Vee tells the supervisor that it is up and still running, so the
//! supervisor can restart it when it stops. On Linux the supervisor is systemd (see
//! `linux::std_watchdog`). The trait uses only `core`.
//!
//! Who calls what: the core calls `feed` in every `Core::tick`.
//!
//! There is no call for "startup is complete". The supervisor's watchdog timer starts when the
//! process does (systemd's `Type=simple`), so the deployment sets the timeout longer than the
//! worst case startup.
//!
//! A deployment with no watchdog (development, SIL, CI) is not an error: `feed` does nothing and
//! returns `Ok`.

use core::error;
use core::fmt;

/// Why a watchdog call failed.
///
/// An enum of our own, as `files::FileError` is, because the flight crates are `no_std`, so they
/// cannot name std's error. Where std has a kind with the same meaning the variant has its name
/// (`NotFound`, `PermissionDenied`, `ConnectionRefused`); `QueueFull` is std's `WouldBlock` said
/// the way it applies here. Each `Display` text says what is wrong in watchdog terms, because it
/// is what ends up in the event. Only the kinds a flight system needs are listed, and any other
/// failure is `Other`. Add a variant when something needs to tell one apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogError {
    /// The supervisor's socket is not there.
    NotFound,
    /// The process is not allowed to send to the supervisor's socket.
    PermissionDenied,
    /// Nothing is listening on the supervisor's socket.
    ConnectionRefused,
    /// The supervisor's queue is full (it is busy or behind), and the call did not wait (it never
    /// waits). Transient: the next call will probably get through.
    QueueFull,
    /// A failure with no kind of its own above.
    Other,
}

impl fmt::Display for WatchdogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WatchdogError::NotFound => "supervisor socket not found",
            WatchdogError::PermissionDenied => "not allowed to send to the supervisor socket",
            WatchdogError::ConnectionRefused => "nothing is listening on the supervisor socket",
            WatchdogError::QueueFull => "supervisor queue is full",
            WatchdogError::Other => "other error",
        })
    }
}

impl error::Error for WatchdogError {}

/// Every method takes `&self`, as `Files` and `Clock` do: the implementation keeps whatever state
/// it needs behind interior mutability, so one value can be shared.
///
/// Every method takes `&self`, as `Files` and `Clock` do: the implementation keeps whatever state
/// it needs behind interior mutability, so one value can be shared.
///
/// `feed` may not wait: it is called in the tick loop. An `Err` means the supervisor was not
/// told. The caller reports it and tries again at the next call, so a failure is never fatal.
pub trait Watchdog {
    /// Tells the supervisor that Vee is alive. It is called every tick, so an implementation must
    /// be cheap: a send is fine, a wait is not. It returns `Ok` without sending when there is no
    /// supervisor to tell, and while the process is panicking, so a hung panic hook cannot be
    /// hidden by a main loop that keeps ticking.
    fn feed(&self) -> Result<(), WatchdogError>;
}

#[cfg(test)]
mod tests {
    use super as watchdog;

    #[test]
    fn each_error_has_its_own_text() {
        let texts = [
            watchdog::WatchdogError::NotFound,
            watchdog::WatchdogError::PermissionDenied,
            watchdog::WatchdogError::ConnectionRefused,
            watchdog::WatchdogError::QueueFull,
            watchdog::WatchdogError::Other,
        ]
        .map(|error| error.to_string());
        assert_eq!(
            texts,
            [
                "supervisor socket not found",
                "not allowed to send to the supervisor socket",
                "nothing is listening on the supervisor socket",
                "supervisor queue is full",
                "other error"
            ]
        );
    }

    #[test]
    fn an_error_can_be_used_as_a_std_error() {
        let error: &dyn core::error::Error = &watchdog::WatchdogError::QueueFull;
        assert_eq!(error.to_string(), "supervisor queue is full");
        assert!(error.source().is_none());
    }
}
