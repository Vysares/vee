//! What modules are told about how the previous run ended. The core fills this in at startup
//! from the reset log, and modules read it through `ctx.reset`.

/// Why a clean shutdown happened. An unclean end has no reason: it is inferred from a boot with
/// no shutdown after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    Commanded,
    Signal,
}

/// How the previous run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviousEnd {
    /// The previous run shut down cleanly, for this reason.
    Clean(ShutdownReason),
    /// There is no record of a clean end. The previous run crashed, was killed, or lost power.
    /// Also the value on the very first boot, when there is no previous run at all.
    Unknown,
}

/// The reset information for this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetInfo {
    /// How many times Vee has booted, counting this run. The first boot is 1.
    pub count: u32,
    pub previous: PreviousEnd,
    /// How many runs in a row, most recent first, ended with no clean shutdown. 0 if the
    /// previous run ended cleanly. Limited by how much of the log is read at startup.
    pub consecutive_unclean: u32,
}

impl ResetInfo {
    /// The info for a log with no usable history: the first boot.
    pub const fn first_boot() -> Self {
        ResetInfo {
            count: 1,
            previous: PreviousEnd::Unknown,
            consecutive_unclean: 0,
        }
    }
}
