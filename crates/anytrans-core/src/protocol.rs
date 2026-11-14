//! Wire format.
//!
//! Every message is a frame: a little-endian `u32` length followed by that many
//! bytes of postcard.
//!
//! Pairing (`ALPN_PAIR`), one bidirectional stream:
//!   joiner -> [`PairRequest`], host -> [`PairResponse`].
//!
//! Transfer (`ALPN_XFER`):
//!   * Control stream (the first bidirectional stream):
//!     sender -> [`Offer`], receiver -> [`Reply`], then once every file is
//!     through, sender -> [`Control::Finish`], receiver -> [`Control::Finished`].
//!   * One further bidirectional stream per file:
//!     sender -> [`FileHeader`], the file's bytes from `offset` to the end, then
//!     the 32-byte BLAKE3 hash of the whole file. Receiver -> one status byte,
//!     [`FILE_OK`] or [`FILE_MISMATCH`].

use std::{io::Read, path::PathBuf};

use anyhow::{bail, Context, Result};
use iroh::{EndpointAddr, EndpointId};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const ALPN_PAIR: &[u8] = b"anytrans/pair/1";
pub const ALPN_XFER: &[u8] = b"anytrans/xfer/1";

pub const FILE_OK: u8 = 1;
pub const FILE_MISMATCH: u8 = 0;

/// Size of the buffers file data is read and written through.
pub const CHUNK: usize = 256 * 1024;

const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    File,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Path relative to the destination, `/`-separated.
    pub rel_path: String,
    pub kind: ItemKind,
    /// Size in bytes; 0 for directories.
    pub size: u64,
    /// Modification time in seconds since the Unix epoch; 0 if unknown.
    pub mtime: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Offer {
    pub items: Vec<Item>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    /// One entry per offered item: how many bytes of it the receiver already
    /// holds. Equal to the item's size when the file needs no sending at all.
    Accept {
        offsets: Vec<u64>,
    },
    Reject {
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileHeader {
    pub index: u32,
    pub offset: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Control {
    Finish,
    Finished,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairRequest {
    pub secret: [u8; 32],
    pub name: String,
    /// Where the joiner can be reached, so the host can dial it right away.
    pub addr: EndpointAddr,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PairResponse {
    Ok { name: String },
    Denied,
}

pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = postcard::to_stdvec(msg)?;
    if body.len() > MAX_FRAME {
        bail!("message too large ({} bytes)", body.len());
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

pub async fn read_frame<R, T>(r: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await.context("connection lost")?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        bail!("message too large ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await.context("connection lost")?;
    Ok(postcard::from_bytes(&body)?)
}

/// Identifies a transfer by who sends it and exactly what is sent, so that
/// sending the same unchanged files again resumes instead of starting over.
pub fn transfer_id(sender: &EndpointId, items: &[Item]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(sender.as_bytes());
    hasher.update(&postcard::to_stdvec(items).expect("items serialize"));
    hasher.finalize().to_hex().to_string()
}

/// Hashes the first `len` bytes of a file, returning the hasher so the caller
/// can keep feeding it the rest.
pub(crate) async fn hash_prefix(path: PathBuf, len: u64) -> Result<blake3::Hasher> {
    tokio::task::spawn_blocking(move || {
        let mut hasher = blake3::Hasher::new();
        if len == 0 {
            return Ok(hasher);
        }
        let mut file = std::fs::File::open(&path)?;
        let mut buf = vec![0u8; CHUNK];
        let mut left = len;
        while left > 0 {
            let want = left.min(CHUNK as u64) as usize;
            let n = file.read(&mut buf[..want])?;
            if n == 0 {
                bail!("{} is shorter than expected", path.display());
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
        Ok(hasher)
    })
    .await?
}
