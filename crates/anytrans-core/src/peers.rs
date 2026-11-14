//! The list of paired devices, persisted as `peers.json`.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{bail, Context, Result};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub id: EndpointId,
    pub name: String,
}

pub(crate) struct PeerStore {
    path: PathBuf,
    peers: Mutex<Vec<Peer>>,
}

impl PeerStore {
    pub(crate) fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("peers.json");
        let peers = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is corrupt", path.display()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        Ok(Self {
            path,
            peers: Mutex::new(peers),
        })
    }

    pub(crate) fn list(&self) -> Vec<Peer> {
        self.peers.lock().unwrap().clone()
    }

    pub(crate) fn get(&self, id: &EndpointId) -> Option<Peer> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .find(|p| &p.id == id)
            .cloned()
    }

    /// Adds the peer, or updates its name if it is already paired.
    pub(crate) fn upsert(&self, peer: Peer) -> Result<()> {
        let mut peers = self.peers.lock().unwrap();
        match peers.iter_mut().find(|p| p.id == peer.id) {
            Some(existing) => existing.name = peer.name,
            None => peers.push(peer),
        }
        self.save(&peers)
    }

    pub(crate) fn remove(&self, id: &EndpointId) -> Result<bool> {
        let mut peers = self.peers.lock().unwrap();
        let before = peers.len();
        peers.retain(|p| &p.id != id);
        let removed = peers.len() != before;
        if removed {
            self.save(&peers)?;
        }
        Ok(removed)
    }

    /// Finds a peer by exact name (case-insensitive) or by id prefix.
    pub(crate) fn find(&self, query: &str) -> Result<Peer> {
        let peers = self.peers.lock().unwrap();
        let q = query.to_lowercase();
        let by_name: Vec<&Peer> = peers
            .iter()
            .filter(|p| p.name.to_lowercase() == q)
            .collect();
        let matches = if by_name.is_empty() {
            peers
                .iter()
                .filter(|p| p.id.to_string().starts_with(&q))
                .collect()
        } else {
            by_name
        };
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] => bail!("no paired device matches '{query}'"),
            _ => bail!("'{query}' matches more than one device; use its id"),
        }
    }

    fn save(&self, peers: &[Peer]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(peers)?)?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot write {}", self.path.display()))
    }
}
