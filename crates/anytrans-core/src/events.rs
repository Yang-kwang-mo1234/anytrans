//! Events a [`Node`](crate::Node) emits for a UI or CLI to display.

use std::{
    net::IpAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use iroh::{endpoint::Connection, TransportAddr};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::peers::Peer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Send,
    Receive,
}

/// How the bytes of a connection are currently travelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// Directly, to an address on the local network.
    Lan,
    /// Directly, across the internet.
    Direct,
    /// Through a relay server.
    Relay,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Paired {
        peer: Peer,
    },
    TransferStarted {
        id: String,
        peer: String,
        direction: Direction,
        files: usize,
        total_bytes: u64,
        /// Bytes already on the receiving device from an earlier attempt.
        /// Only known on the receiving side; senders report 0 here.
        resumed_bytes: u64,
    },
    Progress {
        id: String,
        done_bytes: u64,
        total_bytes: u64,
        path: PathKind,
    },
    TransferFinished {
        id: String,
        direction: Direction,
        /// Where the top-level files and folders were saved (receiving side only).
        saved: Vec<PathBuf>,
    },
    TransferFailed {
        id: String,
        direction: Direction,
        error: String,
    },
}

const EMIT_INTERVAL: Duration = Duration::from_millis(150);

/// Byte counter for one transfer that emits throttled [`Event::Progress`].
pub(crate) struct Progress {
    id: String,
    total: u64,
    done: AtomicU64,
    last_emit: Mutex<Instant>,
    events: broadcast::Sender<Event>,
}

impl Progress {
    pub(crate) fn new(id: String, total: u64, events: broadcast::Sender<Event>) -> Self {
        Self {
            id,
            total,
            done: AtomicU64::new(0),
            last_emit: Mutex::new(Instant::now()),
            events,
        }
    }

    pub(crate) fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    pub(crate) fn set(&self, done: u64) {
        self.done.store(done, Ordering::Relaxed);
    }

    pub(crate) fn add(&self, n: u64, conn: &Connection) {
        let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
        let mut last = self.last_emit.lock().unwrap();
        if done < self.total && last.elapsed() < EMIT_INTERVAL {
            return;
        }
        *last = Instant::now();
        drop(last);
        let _ = self.events.send(Event::Progress {
            id: self.id.clone(),
            done_bytes: done,
            total_bytes: self.total,
            path: path_kind(conn),
        });
    }

    /// Takes back bytes that were counted but then discarded (a failed file).
    pub(crate) fn sub(&self, n: u64) {
        self.done.fetch_sub(n, Ordering::Relaxed);
    }
}

pub(crate) fn path_kind(conn: &Connection) -> PathKind {
    let paths = conn.paths();
    let Some(path) = paths.iter().find(|p| p.is_selected()) else {
        return PathKind::Unknown;
    };
    match path.remote_addr() {
        TransportAddr::Relay(_) => PathKind::Relay,
        TransportAddr::Ip(addr) if is_local(addr.ip()) => PathKind::Lan,
        TransportAddr::Ip(_) => PathKind::Direct,
        _ => PathKind::Unknown,
    }
}

fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
            }
        },
    }
}
