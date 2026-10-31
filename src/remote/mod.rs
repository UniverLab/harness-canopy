//! Remote control: GitHub identity, the host key, pairing, the device
//! registry, trusted guest profiles and the relay connection.
//!
//! The relay admits only hosts signed in with the same GitHub account. This
//! module holds the `canopy remote login|logout|status|on|off|pair|devices|
//! revoke` surface plus the daemon's relay client; trusted GitHub profiles
//! grant guests some or all of the hub's sessions.

pub mod auth;
pub mod bridge;
pub mod cli;
pub mod connection;
pub mod devices;
pub mod hostkey;
pub mod live;
pub mod metrics;
pub mod pairing;
pub mod panel;
pub mod peer;
pub mod qr;
pub mod relay;
pub mod signaling;
pub mod trust;
