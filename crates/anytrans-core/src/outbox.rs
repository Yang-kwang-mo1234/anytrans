//! Sends that were started but have not finished, persisted as `outbox.json`
//! so they can be picked up again after the app restarts.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSend {
    pub to: EndpointId,
    pub paths: Vec<PathBuf>,
}

pub(crate) struct Outbox {
    path: PathBuf,
    pending: Mutex<Vec<PendingSend>>,
}

impl Outbox {
    /// A corrupt file is treated as empty: losing the list only costs the
    /// user a manual re-send, which is better than refusing to start.
    pub(crate) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("outbox.json");
        let pending = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            pending: Mutex::new(pending),
        }
    }

    pub(crate) fn list(&self) -> Vec<PendingSend> {
        self.pending.lock().unwrap().clone()
    }

    pub(crate) fn add(&self, send: PendingSend) -> Result<()> {
        let mut pending = self.pending.lock().unwrap();
        if pending.contains(&send) {
            return Ok(());
        }
        pending.push(send);
        self.save(&pending)
    }

    pub(crate) fn remove(&self, send: &PendingSend) -> Result<()> {
        let mut pending = self.pending.lock().unwrap();
        let before = pending.len();
        pending.retain(|p| p != send);
        if pending.len() == before {
            return Ok(());
        }
        self.save(&pending)
    }

    fn save(&self, pending: &[PendingSend]) -> Result<()> {
        if pending.is_empty() {
            return match fs::remove_file(&self.path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            };
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(pending)?)?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("cannot write {}", self.path.display()))
    }
}
