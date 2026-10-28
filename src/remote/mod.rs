//! Owner-only remote control: GitHub identity, the host key, pairing,
//! the device registry and the relay connection.
//!
//! The relay admits only hosts signed in with the same GitHub account. This
//! module holds the `canopy remote login|logout|status|on|off|pair|devices|
//! revoke` surface plus the daemon's relay client.

pub mod auth;
pub mod bridge;
pub mod cli;
pub mod connection;
pub mod devices;
pub mod hostkey;
pub mod live;
pub mod pairing;
pub mod panel;
pub mod peer;
pub mod qr;
pub mod relay;
pub mod signaling;
