//! Deny-by-default `falcone:host@0.1.0` capability gate (WAS-02.t4, FR-WAS-03).
//!
//! Nothing here can be switched off: there is no setting, environment variable or feature that
//! disables the import check, the grant checks or the egress checks. Authority comes only from
//! the verified grants (`grant::GrantSet`); guest arguments select within them and never widen
//! them. WASI filesystem, socket and environment interfaces are never linked.

pub mod bindings;
pub mod call;
pub mod egress;
pub mod grant;
pub mod http;
pub mod limits;
pub mod net;
pub mod runtime;

/// The shared failure vocabulary of `worker/wit/host.wit` (`cancellation.host-error`). It never
/// carries credentials or binding details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError {
    Denied,
    LimitExceeded,
    Timeout,
    Cancelled,
}
