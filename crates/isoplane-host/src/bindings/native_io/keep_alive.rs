//! TCP keep-alive on a guest's outbound socket: the bounds a request is
//! clamped to, and the record the host applies to its own socket.

use std::time::Duration;

use isoplane_abi::native_io::{DurationNanos, Error, KeepAlive, KeepAliveChange, ProbeCount};
use tokio::net::TcpStream;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// The host's kernel probes a healthy idle connection without waking the
/// guest, so a parked or hibernated instance occupies no worker while the
/// host sends one probe per idle period on each such connection. The floor
/// bounds that rate.
const MIN_IDLE_SECS: u64 = 10;

/// Linux and macOS take whole seconds, so one is the shortest interval the
/// host can honour on each of them. Probes this close together go only to a
/// peer that has stopped answering, and `MAX_PROBES` of them end the
/// connection.
const MIN_INTERVAL_SECS: u64 = 1;

/// Linux refuses a longer idle time or interval, and a guest's request above
/// what the host OS takes must still succeed.
const MAX_TIMING_SECS: u64 = 32_767;

/// Linux refuses more probes.
const MAX_PROBES: u32 = 127;

/// Keep-alive as the host applies it to one socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Settings {
    enabled: bool,
    idle_secs: u64,
    interval_secs: u64,
    probes: u32,
}

/// Linux's stock timings. A guest can read its settings before the host has
/// a socket to ask, so this record answers every read; starting it from fixed
/// values also gives a guest that only switches keep-alive on the same
/// probing on every host OS.
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_secs: 7_200,
            interval_secs: 75,
            probes: 9,
        }
    }
}

impl Settings {
    /// These settings after `change`, with a timing rounded up to whole
    /// seconds and every value clamped to what the host allows. Fails on a
    /// zero timing or probe count, which `wasi:sockets` defines as invalid.
    pub(super) fn with(self, change: KeepAliveChange) -> Result<Self, Error> {
        Ok(match change {
            KeepAliveChange::Enabled(enabled) => Self { enabled, ..self },
            KeepAliveChange::IdleTime(requested) => Self {
                idle_secs: whole_seconds(requested)
                    .ok_or(Error::InvalidArgument("keep-alive idle time is zero"))?
                    .clamp(MIN_IDLE_SECS, MAX_TIMING_SECS),
                ..self
            },
            KeepAliveChange::Interval(requested) => Self {
                interval_secs: whole_seconds(requested)
                    .ok_or(Error::InvalidArgument("keep-alive interval is zero"))?
                    .clamp(MIN_INTERVAL_SECS, MAX_TIMING_SECS),
                ..self
            },
            KeepAliveChange::Count(ProbeCount(0)) => {
                return Err(Error::InvalidArgument("keep-alive probe count is zero"))
            }
            KeepAliveChange::Count(ProbeCount(requested)) => Self {
                probes: requested.min(MAX_PROBES),
                ..self
            },
        })
    }

    /// The settings as a guest reads them back.
    pub(super) fn effective(self) -> KeepAlive {
        KeepAlive {
            enabled: self.enabled,
            idle_time: DurationNanos(self.idle_secs * NANOS_PER_SEC),
            interval: DurationNanos(self.interval_secs * NANOS_PER_SEC),
            count: ProbeCount(self.probes),
        }
    }

    /// Puts these settings on the host's socket.
    pub(super) fn apply(self, stream: &TcpStream) -> std::io::Result<()> {
        let socket = socket2::SockRef::from(stream);
        if !self.enabled {
            return socket.set_keepalive(false);
        }
        // Every timing goes in one call, which also turns keep-alive on:
        // Windows resets a timing left out of it to the system default.
        socket.set_tcp_keepalive(
            &socket2::TcpKeepalive::new()
                .with_time(Duration::from_secs(self.idle_secs))
                .with_interval(Duration::from_secs(self.interval_secs))
                .with_retries(self.probes),
        )
    }
}

/// A requested duration in seconds, rounded up; `None` for zero.
fn whole_seconds(requested: DurationNanos) -> Option<u64> {
    (requested.0 != 0).then(|| requested.0.div_ceil(NANOS_PER_SEC))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(seconds: u64) -> DurationNanos {
        DurationNanos(seconds * NANOS_PER_SEC)
    }

    fn after(change: KeepAliveChange) -> KeepAlive {
        Settings::default()
            .with(change)
            .unwrap_or_else(|error| panic!("{change:?} must be accepted: {error}"))
            .effective()
    }

    #[test]
    fn a_requested_idle_time_is_clamped_to_the_host_bounds() {
        for (requested, effective_secs) in [
            (DurationNanos(1), MIN_IDLE_SECS),
            (seconds(MIN_IDLE_SECS - 1), MIN_IDLE_SECS),
            (seconds(MIN_IDLE_SECS), MIN_IDLE_SECS),
            (
                DurationNanos(MIN_IDLE_SECS * NANOS_PER_SEC + 1),
                MIN_IDLE_SECS + 1,
            ),
            (seconds(MAX_TIMING_SECS), MAX_TIMING_SECS),
            (seconds(MAX_TIMING_SECS + 1), MAX_TIMING_SECS),
            (DurationNanos(u64::MAX), MAX_TIMING_SECS),
        ] {
            assert_eq!(
                after(KeepAliveChange::IdleTime(requested)).idle_time,
                seconds(effective_secs),
                "idle time requested as {requested:?}"
            );
        }
    }

    #[test]
    fn a_requested_interval_is_clamped_to_the_host_bounds() {
        for (requested, effective_secs) in [
            (DurationNanos(1), MIN_INTERVAL_SECS),
            (seconds(MIN_INTERVAL_SECS), MIN_INTERVAL_SECS),
            (DurationNanos(NANOS_PER_SEC + 1), 2),
            (seconds(MAX_TIMING_SECS), MAX_TIMING_SECS),
            (seconds(MAX_TIMING_SECS + 1), MAX_TIMING_SECS),
            (DurationNanos(u64::MAX), MAX_TIMING_SECS),
        ] {
            assert_eq!(
                after(KeepAliveChange::Interval(requested)).interval,
                seconds(effective_secs),
                "interval requested as {requested:?}"
            );
        }
    }

    #[test]
    fn a_requested_probe_count_is_clamped_to_the_host_bounds() {
        for (requested, effective) in [
            (1, 1),
            (MAX_PROBES, MAX_PROBES),
            (MAX_PROBES + 1, MAX_PROBES),
            (u32::MAX, MAX_PROBES),
        ] {
            assert_eq!(
                after(KeepAliveChange::Count(ProbeCount(requested))).count,
                ProbeCount(effective),
                "probe count requested as {requested}"
            );
        }
    }

    #[test]
    fn a_zero_timing_or_probe_count_is_refused_as_an_invalid_argument() {
        for change in [
            KeepAliveChange::IdleTime(DurationNanos(0)),
            KeepAliveChange::Interval(DurationNanos(0)),
            KeepAliveChange::Count(ProbeCount(0)),
        ] {
            assert!(
                matches!(
                    Settings::default().with(change),
                    Err(Error::InvalidArgument(_))
                ),
                "{change:?} must be refused"
            );
        }
    }

    #[test]
    fn a_change_leaves_the_other_settings_as_they_were() {
        let settings = Settings::default()
            .with(KeepAliveChange::IdleTime(seconds(30)))
            .and_then(|settings| settings.with(KeepAliveChange::Enabled(true)))
            .expect("both changes are within bounds");
        assert_eq!(
            settings.effective(),
            KeepAlive {
                enabled: true,
                idle_time: seconds(30),
                ..Settings::default().effective()
            }
        );
    }
}
