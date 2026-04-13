//! Device discovery for physical mobile devices.
//!
//! Currently provides a polling-based mDNS scanner stub.  Full `mdns-sd`
//! integration is deferred until Windows support is confirmed.

pub mod cloud_registry;
pub mod mdns_scanner;
