//! Device pairing and resumable file transfer over iroh.
//!
//! [`Node`] is the whole public surface: start one per device, pair it with
//! other devices once, then call [`Node::send`]. Incoming transfers from paired
//! devices are saved to the download directory automatically.

pub mod events;
pub mod fs_safe;
mod identity;
mod node;
mod outbox;
mod pairing;
mod peers;
pub mod protocol;
mod recv;
mod send;

pub use events::{Direction, Event, PathKind};
pub use iroh::{EndpointAddr, EndpointId};
pub use node::{Node, NodeOptions};
pub use outbox::PendingSend;
pub use peers::Peer;
pub use send::{build_manifest, Manifest};
