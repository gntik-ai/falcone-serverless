//! Per-call byte ceilings and deadline of the host contract (`worker/wit/host.wit:3-6`).

use std::time::Duration;

use super::HostError;

pub const MAX_GUEST_TO_HOST_BYTES: usize = 1_048_576;
pub const MAX_HOST_TO_GUEST_BYTES: usize = 1_048_576;
pub const HOST_CALL_TIMEOUT: Duration = Duration::from_millis(30_000);

/// The ceilings count the aggregate serialized call, metadata included
/// (`worker/wit/README.md`, per-call ceilings). These are the canonical-ABI bytes that frame
/// the data: a string or list is a pointer and a length, a resource handle is one `u32`, a
/// `name`/`value` record is two such pairs. Contents are counted on top.
pub const STRING_FRAME_BYTES: usize = 8;
pub const HANDLE_FRAME_BYTES: usize = 4;
pub const U64_FRAME_BYTES: usize = 8;
pub const RECORD_FRAME_BYTES: usize = 16;
/// `http.request`: url, method, header list and body, each a pointer and length.
pub const HTTP_REQUEST_FRAME_BYTES: usize = 4 * STRING_FRAME_BYTES;
/// A `result<_, host-error>` is a discriminator padded to the 4-byte alignment of its payload.
pub const RESULT_FRAME_BYTES: usize = 4;
/// `http.response` inside its result: the discriminator, the status padded to 4 bytes, the
/// header list and the body list.
pub const HTTP_RESPONSE_FRAME_BYTES: usize = RESULT_FRAME_BYTES + 4 + 2 * STRING_FRAME_BYTES;

/// Serialized bytes of a string or byte list holding `len` content bytes.
pub const fn string_bytes(len: usize) -> usize {
    len.saturating_add(STRING_FRAME_BYTES)
}

/// Serialized bytes of a `result<list<u8>, host-error>` returning `len` content bytes.
pub const fn result_bytes(len: usize) -> usize {
    string_bytes(len).saturating_add(RESULT_FRAME_BYTES)
}

/// Serialized bytes of one `name`/`value` record: the record plus both strings' contents.
pub const fn pair_bytes(name: usize, value: usize) -> usize {
    name.saturating_add(value)
        .saturating_add(RECORD_FRAME_BYTES)
}

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
