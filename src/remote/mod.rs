//! Remote control: GitHub identity, the host key, pairing, the device
//! registry, trusted guest profiles and the relay connection.
//!
//! The relay admits only hosts signed in with the same GitHub account. This
//! module holds the `canopy remote login|logout|status|on|off|pair|devices|
//! revoke|invite|invites|uninvite` surface plus the daemon's relay client;
//! trusted GitHub profiles grant guests some or all of the hub's sessions.

pub mod auth;
pub mod bridge;
pub mod cli;
pub mod connection;
mod connection_registry;
pub mod devices;
pub mod guest;
mod guest_allow;
pub mod hostkey;
pub mod invite;
pub mod label;
pub mod live;
pub mod metrics;
pub mod pairing;
pub mod panel;
pub mod peer;
pub mod peer_ice;
pub mod proxy;
pub mod qr;
#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod reconnect_tests;
pub mod relay;
mod relay_offer;
pub mod relay_socket;
pub mod signaling;
pub mod trust;
