//! Owner-only remote control: GitHub identity (device flow) and relay config.
//!
//! The relay admits only hosts signed in with the same GitHub account. This
//! module holds the `canopy remote login|logout|status` surface; the relay
//! connection itself is a later spec.

pub mod auth;
pub mod cli;
