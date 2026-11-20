//! The Linux `Watchdog`: tells systemd that Vee is alive (`WATCHDOG=1`), with the `sd_notify`
//! protocol. There is no library to link: a message is the text, sent as one datagram to the Unix
//! socket systemd names. The trait is in `watchdog`.
//!
//! How systemd sets it up (see `sd_notify(3)` and `systemd.service(5)`), through the
//! environment of the service:
//! - `NOTIFY_SOCKET`: where to send. A path (starts with `/`), or an abstract socket name (starts
//!   with `@`). Not set: Vee is not run by systemd (development, SIL, CI), and nothing is sent.
//! - `WATCHDOG_USEC`: the timeout, in microseconds, from `WatchdogSec=`. The service must send
//!   `WATCHDOG=1` at least this often, or systemd kills it. Not set (or 0): no watchdog, so `feed`
//!   sends nothing. The timer starts when the process does (`Type=simple`), so `WatchdogSec=` must
//!   be longer than the worst case startup. Setting it makes `NotifyAccess=` default to `main`,
//!   which is what lets our messages in. Its value is not used beyond that: `feed` sends on every
//!   call, and the core calls it every tick, so the keep-alives come far more often than any
//!   timeout needs. (A send costs about 2.5 microseconds.)
//! - `WATCHDOG_PID`: when set, the process the watchdog is for. If it is not us (a child of the
//!   service, say), there is no watchdog for us.
//!
//! Who calls what: the executive's start function calls `from_env` once (it allocates, so it is
//! before `started`). The core calls `feed` in every tick.
//!
//! Nothing here waits. The socket is non-blocking: if systemd's queue is full a send fails with
//! `QueueFull`, and the next call tries again.

use crate::linux::std_panic;
use crate::watchdog;
use std::fmt;
use std::io;
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::net;

/// The keep-alive message.
const KEEP_ALIVE: &[u8] = b"WATCHDOG=1";

/// Why `from_env` could not make a watchdog. The environment is the deployment's configuration,
/// so a bad value is reported and not guessed at.
#[derive(Debug)]
pub enum StartError {
    /// `NOTIFY_SOCKET` does not start with `/` or `@`, or has nothing after the `@`.
    InvalidSocketName,
    /// `WATCHDOG_USEC` is not a whole number.
    InvalidWatchdogUsec,
    /// `WATCHDOG_PID` is not a whole number.
    InvalidWatchdogPid,
    /// The socket could not be made, or `NOTIFY_SOCKET` is not a name the OS accepts.
    Socket(io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::InvalidSocketName => f.write_str("NOTIFY_SOCKET is not a path or an @name"),
            StartError::InvalidWatchdogUsec => f.write_str("WATCHDOG_USEC is not a number"),
            StartError::InvalidWatchdogPid => f.write_str("WATCHDOG_PID is not a number"),
            StartError::Socket(error) => write!(f, "cannot set up the notify socket: {error}"),
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StartError::Socket(error) => Some(error),
            _ => None,
        }
    }
}

/// Where systemd listens, as `NOTIFY_SOCKET` names it.
#[derive(Debug, PartialEq, Eq)]
enum Address {
    Path(String),
    /// The name after the `@`.
    Abstract(String),
}

/// Reads the three variables' values (`None` for a variable that is not set), checks all of
/// them, and says where keep-alives go: `None` if there is no watchdog for this process (no
/// `WATCHDOG_USEC`, or 0, or a `WATCHDOG_PID` that is not ours, or no `NOTIFY_SOCKET` to send
/// to). `own_pid` is this process's id, to compare with `WATCHDOG_PID`.
///
/// It takes plain values, so a test can give it any of them without setting environment
/// variables.
fn keep_alive_address(
    notify_socket: Option<&str>,
    watchdog_usec: Option<&str>,
    watchdog_pid: Option<&str>,
    own_pid: u32,
) -> Result<Option<Address>, StartError> {
    let address = notify_socket.map(parse_address).transpose()?;

    // 0 means no watchdog, as systemd's own `sd_watchdog_enabled` has it.
    let mut enabled = match watchdog_usec {
        None => false,
        Some(text) => match text.parse::<u64>() {
            Ok(usec) => usec > 0,
            Err(_) => return Err(StartError::InvalidWatchdogUsec),
        },
    };

    // The watchdog belongs to the process in `WATCHDOG_PID`, if that is set.
    if let Some(text) = watchdog_pid {
        let pid = text
            .parse::<u32>()
            .map_err(|_| StartError::InvalidWatchdogPid)?;
        if pid != own_pid {
            enabled = false;
        }
    }

    Ok(if enabled { address } else { None })
}

fn parse_address(name: &str) -> Result<Address, StartError> {
    if let Some(abstract_name) = name.strip_prefix('@') {
        if abstract_name.is_empty() {
            return Err(StartError::InvalidSocketName);
        }
        return Ok(Address::Abstract(abstract_name.to_string()));
    }
    if name.starts_with('/') {
        return Ok(Address::Path(name.to_string()));
    }
    Err(StartError::InvalidSocketName)
}

/// The socket to send on and where to send. Made once, so sending needs no allocation.
struct Link {
    socket: net::UnixDatagram,
    address: net::SocketAddr,
}

/// A `Watchdog` for systemd. It has no state that changes, so every method takes `&self` freely.
pub struct StdWatchdog {
    /// `None`: there is no watchdog for this process, so `feed` does nothing.
    link: Option<Link>,
}

impl StdWatchdog {
    /// Reads `NOTIFY_SOCKET`, `WATCHDOG_USEC` and `WATCHDOG_PID` from the environment of this
    /// process and makes the watchdog. Allocates (reads strings, makes a socket): call it at
    /// startup.
    pub fn from_env() -> Result<Self, StartError> {
        let address = keep_alive_address(
            std::env::var("NOTIFY_SOCKET").ok().as_deref(),
            std::env::var("WATCHDOG_USEC").ok().as_deref(),
            std::env::var("WATCHDOG_PID").ok().as_deref(),
            std::process::id(),
        )?;
        Self::new(address)
    }

    /// A watchdog that does nothing: `feed` always returns `Ok` and sends nothing, as when Vee is
    /// not run by systemd. It is what the executive uses when `from_env` fails, because startup
    /// does not fail: it reports the `StartError` and carries on. If systemd really is waiting
    /// for keep-alives it will restart the process, which is what the watchdog is for.
    pub fn disabled() -> Self {
        StdWatchdog { link: None }
    }

    fn new(address: Option<Address>) -> Result<Self, StartError> {
        let link = match address {
            None => None,
            Some(address) => Some(Link {
                socket: new_socket()?,
                address: socket_address(&address)?,
            }),
        };
        Ok(StdWatchdog { link })
    }

    /// `feed`, with the panic flag given, so a test can try both.
    fn feed_unless(&self, panicking: bool) -> Result<(), watchdog::WatchdogError> {
        let Some(link) = &self.link else {
            return Ok(()); // no watchdog for this process
        };
        if panicking {
            return Ok(()); // see `Watchdog::feed`
        }
        send(link, KEEP_ALIVE)
    }
}

impl watchdog::Watchdog for StdWatchdog {
    fn feed(&self) -> Result<(), watchdog::WatchdogError> {
        self.feed_unless(std_panic::is_panicking())
    }
}

/// A datagram socket that never waits.
fn new_socket() -> Result<net::UnixDatagram, StartError> {
    let socket = net::UnixDatagram::unbound().map_err(StartError::Socket)?;
    socket.set_nonblocking(true).map_err(StartError::Socket)?;
    Ok(socket)
}

fn socket_address(address: &Address) -> Result<net::SocketAddr, StartError> {
    match address {
        Address::Path(path) => net::SocketAddr::from_pathname(path),
        Address::Abstract(name) => net::SocketAddr::from_abstract_name(name),
    }
    .map_err(StartError::Socket)
}

/// One datagram, in one call, with no allocation.
fn send(link: &Link, message: &[u8]) -> Result<(), watchdog::WatchdogError> {
    link.socket.send_to_addr(message, &link.address)?;
    Ok(())
}

impl From<io::Error> for watchdog::WatchdogError {
    /// The kinds the trait lists keep their meaning; `WouldBlock` is "the queue is full" here.
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => watchdog::WatchdogError::NotFound,
            io::ErrorKind::PermissionDenied => watchdog::WatchdogError::PermissionDenied,
            io::ErrorKind::ConnectionRefused => watchdog::WatchdogError::ConnectionRefused,
            io::ErrorKind::WouldBlock => watchdog::WatchdogError::QueueFull,
            _ => watchdog::WatchdogError::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super as std_watchdog;
    use crate::linux::std_alloc_guard;
    use crate::watchdog::Watchdog as _;
    use crate::watchdog::WatchdogError;
    use std::fs;
    use std::io;
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net;
    use std::path;

    const OWN_PID: u32 = 4242;

    // --- the environment, as plain values ---

    fn address(
        socket: Option<&str>,
        usec: Option<&str>,
        pid: Option<&str>,
    ) -> Result<Option<std_watchdog::Address>, std_watchdog::StartError> {
        std_watchdog::keep_alive_address(socket, usec, pid, OWN_PID)
    }

    fn path(name: &str) -> Option<std_watchdog::Address> {
        Some(std_watchdog::Address::Path(name.to_string()))
    }

    #[test]
    fn nothing_set_means_no_watchdog() {
        assert_eq!(address(None, None, None).unwrap(), None);
    }

    #[test]
    fn a_path_and_an_abstract_name_are_told_apart() {
        assert_eq!(
            address(Some("/run/systemd/notify"), Some("1000"), None).unwrap(),
            path("/run/systemd/notify")
        );
        assert_eq!(
            address(Some("@vee-test"), Some("1000"), None).unwrap(),
            Some(std_watchdog::Address::Abstract("vee-test".to_string()))
        );
    }

    #[test]
    fn a_bad_socket_name_is_an_error_even_with_no_watchdog() {
        for bad in ["", "relative/path", "@"] {
            assert!(
                matches!(
                    address(Some(bad), None, None),
                    Err(std_watchdog::StartError::InvalidSocketName)
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_watchdog_needs_a_timeout_above_zero_and_a_socket_to_send_to() {
        assert_eq!(address(Some("/s"), None, None).unwrap(), None);
        assert_eq!(address(Some("/s"), Some("0"), None).unwrap(), None);
        assert_eq!(address(None, Some("1000"), None).unwrap(), None);
        assert_eq!(address(Some("/s"), Some("1000"), None).unwrap(), path("/s"));
        assert!(matches!(
            address(Some("/s"), Some("soon"), None),
            Err(std_watchdog::StartError::InvalidWatchdogUsec)
        ));
    }

    #[test]
    fn a_watchdog_for_another_process_is_not_ours() {
        assert_eq!(
            address(Some("/s"), Some("1000"), Some("4242")).unwrap(),
            path("/s")
        );
        assert_eq!(address(Some("/s"), Some("1000"), Some("17")).unwrap(), None);
        assert!(matches!(
            address(Some("/s"), Some("1000"), Some("me")),
            Err(std_watchdog::StartError::InvalidWatchdogPid)
        ));
    }

    // --- a real socket ---

    /// A new empty directory for one test, deleted when it goes out of scope. Each test passes its
    /// own name, so tests running in parallel do not share a directory.
    struct TempDir {
        dir: path::PathBuf,
    }

    #[allow(
        clippy::let_underscore_must_use,
        reason = "test cleanup: a failure to delete, or nothing to delete, does not matter"
    )]
    fn remove_dir(dir: &path::Path) {
        let _ = fs::remove_dir_all(dir);
    }

    impl TempDir {
        fn new(test_name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("vee-watchdog-{}-{test_name}", std::process::id()));
            remove_dir(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir { dir }
        }

        fn path(&self, name: &str) -> String {
            self.dir.join(name).to_str().unwrap().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            remove_dir(&self.dir);
        }
    }

    /// A watchdog sending to `address`.
    fn watchdog_to(address: std_watchdog::Address) -> std_watchdog::StdWatchdog {
        std_watchdog::StdWatchdog::new(Some(address)).unwrap()
    }

    /// What the receiver has been sent so far, one string per datagram.
    fn received(receiver: &net::UnixDatagram) -> Vec<String> {
        let mut messages = Vec::new();
        let mut buf = [0_u8; 64];
        loop {
            match receiver.recv(&mut buf) {
                Ok(len) => messages.push(String::from_utf8(buf[..len].to_vec()).unwrap()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return messages,
                Err(error) => panic!("receive failed: {error}"),
            }
        }
    }

    /// A receiver bound to a path in `tmp`, and a watchdog sending to it.
    fn pair(tmp: &TempDir) -> (net::UnixDatagram, std_watchdog::StdWatchdog) {
        let path = tmp.path("notify");
        let receiver = net::UnixDatagram::bind(&path).unwrap();
        receiver.set_nonblocking(true).unwrap();
        (receiver, watchdog_to(std_watchdog::Address::Path(path)))
    }

    #[test]
    fn every_feed_sends_a_keep_alive() {
        let tmp = TempDir::new("every_feed_sends");
        let (receiver, dog) = pair(&tmp);
        for _ in 0..3 {
            dog.feed_unless(false).unwrap();
        }
        assert_eq!(received(&receiver), ["WATCHDOG=1"; 3]);
    }

    #[test]
    fn a_panicking_process_sends_nothing() {
        let tmp = TempDir::new("a_panicking_process");
        let (receiver, dog) = pair(&tmp);
        dog.feed_unless(true).unwrap();
        assert!(received(&receiver).is_empty());
        dog.feed_unless(false).unwrap();
        assert_eq!(received(&receiver), ["WATCHDOG=1"]);
    }

    #[test]
    fn a_disabled_watchdog_does_nothing_and_succeeds() {
        let dog = std_watchdog::StdWatchdog::disabled();
        assert_eq!(dog.feed(), Ok(()));
        assert_eq!(dog.feed_unless(false), Ok(()));
    }

    #[test]
    fn an_abstract_socket_works_too() {
        let name = format!("vee-watchdog-{}-abstract", std::process::id());
        let receiver =
            net::UnixDatagram::bind_addr(&net::SocketAddr::from_abstract_name(&name).unwrap())
                .unwrap();
        receiver.set_nonblocking(true).unwrap();
        let dog = watchdog_to(std_watchdog::Address::Abstract(name));
        dog.feed_unless(false).unwrap();
        assert_eq!(received(&receiver), ["WATCHDOG=1"]);
    }

    #[test]
    fn a_missing_socket_is_not_found() {
        let tmp = TempDir::new("a_missing_socket");
        let dog = watchdog_to(std_watchdog::Address::Path(tmp.path("nobody-here")));
        assert_eq!(dog.feed_unless(false), Err(WatchdogError::NotFound));
    }

    #[test]
    fn a_socket_nobody_listens_on_is_refused() {
        let tmp = TempDir::new("a_socket_nobody_listens_on");
        let (receiver, dog) = pair(&tmp);
        drop(receiver); // the file stays, but nothing is bound to it
        assert_eq!(
            dog.feed_unless(false),
            Err(WatchdogError::ConnectionRefused)
        );
    }

    #[test]
    fn a_full_queue_is_queue_full_and_the_next_feed_gets_through_once_there_is_room() {
        let tmp = TempDir::new("a_full_queue");
        let (receiver, dog) = pair(&tmp);
        // Nobody reads, so the receiver's queue fills. The limit is a few hundred messages by
        // default.
        let most_it_could_take = 100_000;
        let mut sent = 0;
        let full = loop {
            match dog.feed_unless(false) {
                Ok(()) => sent += 1,
                Err(error) => break error,
            }
            assert!(sent < most_it_could_take, "the queue never filled");
        };
        assert_eq!(full, WatchdogError::QueueFull);
        assert_eq!(received(&receiver).len(), sent);
        // With room again, the next feed goes out.
        dog.feed_unless(false).unwrap();
        assert_eq!(received(&receiver), ["WATCHDOG=1"]);
    }

    #[test]
    fn io_errors_map_to_the_matching_watchdog_error() {
        let kinds = [
            (io::ErrorKind::NotFound, WatchdogError::NotFound),
            (
                io::ErrorKind::PermissionDenied,
                WatchdogError::PermissionDenied,
            ),
            (
                io::ErrorKind::ConnectionRefused,
                WatchdogError::ConnectionRefused,
            ),
            (io::ErrorKind::WouldBlock, WatchdogError::QueueFull),
            (io::ErrorKind::InvalidInput, WatchdogError::Other),
        ];
        for (kind, expected) in kinds {
            assert_eq!(
                WatchdogError::from(io::Error::from(kind)),
                expected,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_feed_that_sends_does_not_allocate() {
        let tmp = TempDir::new("a_feed_that_sends_does_not_allocate");
        let (receiver, dog) = pair(&tmp);
        let before = std_alloc_guard::allocations_on_this_thread();
        dog.feed_unless(false).unwrap();
        let after = std_alloc_guard::allocations_on_this_thread();
        assert_eq!(after, before);
        assert_eq!(received(&receiver).len(), 1);
    }
}
