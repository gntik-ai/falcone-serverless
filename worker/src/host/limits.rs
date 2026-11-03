//! Per-call byte ceilings and deadline of the host contract (`worker/wit/host.wit:3-6`).

use std::time::Duration;

use super::HostError;

pub const MAX_GUEST_TO_HOST_BYTES: usize = 1_048_576;
pub const MAX_HOST_TO_GUEST_BYTES: usize = 1_048_576;
pub const HOST_CALL_TIMEOUT: Duration = Duration::from_millis(30_000);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostLimits {
    guest_to_host: usize,
    host_to_guest: usize,
    call_timeout: Duration,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self::new(
            MAX_GUEST_TO_HOST_BYTES,
            MAX_HOST_TO_GUEST_BYTES,
            HOST_CALL_TIMEOUT,
        )
    }
}

impl HostLimits {
    /// Tighter limits are for tests and profiles; the contract ceilings are the defaults.
    pub const fn new(guest_to_host: usize, host_to_guest: usize, call_timeout: Duration) -> Self {
        Self {
            guest_to_host,
            host_to_guest,
            call_timeout,
        }
    }

    pub const fn guest_to_host_bytes(&self) -> usize {
        self.guest_to_host
    }

    pub const fn host_to_guest_bytes(&self) -> usize {
        self.host_to_guest
    }

    pub const fn call_timeout(&self) -> Duration {
        self.call_timeout
    }

    /// The aggregate of every guest-supplied part of one call must fit; an overflowing sum
    /// is over the limit.
    pub fn check_request(&self, parts: &[usize]) -> Result<(), HostError> {
        let total = parts
            .iter()
            .try_fold(0usize, |total, part| total.checked_add(*part));
        match total {
            Some(total) if total <= self.guest_to_host => Ok(()),
            _ => Err(HostError::LimitExceeded),
        }
    }

    pub fn check_response(&self, bytes: usize) -> Result<(), HostError> {
        if bytes <= self.host_to_guest {
            Ok(())
        } else {
            Err(HostError::LimitExceeded)
        }
    }
}
