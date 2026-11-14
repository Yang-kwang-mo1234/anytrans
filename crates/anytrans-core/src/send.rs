//! Sending side of a transfer.

use std::{
    collections::HashSet,
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

use anyhow::{anyhow, bail, Context, Result};
use iroh::{endpoint::Connection, Endpoint, EndpointId};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    task::JoinSet,
};

use crate::{
    events::{Direction, Event, Progress},
    node::Shared,
    outbox::PendingSend,
    protocol::{
        hash_prefix, read_frame, transfer_id, write_frame, Control, FileHeader, Item, ItemKind,
        Offer, Reply, ALPN_XFER, CHUNK, FILE_OK,
    },
};

/// Files sent at the same time over one connection.
const PARALLEL_FILES: usize = 4;
/// Attempts in a row that may fail without moving a single byte.
const MAX_STALLED_ATTEMPTS: u32 = 5;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Times one file is sent again from the start after its hash did not match.
const MAX_FILE_RETRIES: u32 = 2;

/// What a transfer consists of, and where each item is on this device.
#[derive(Debug)]
pub struct Manifest {
    pub items: Vec<Item>,
    /// Local path of `items[i]`.
    pub sources: Vec<PathBuf>,
}

/// An error that trying again cannot fix.
#[derive(Debug)]
struct Fatal(String);

impl fmt::Display for Fatal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

fn fatal(msg: impl fmt::Display) -> anyhow::Error {
    Fatal(msg.to_string()).into()
}

/// Lists everything under `paths`. Folders are walked recursively and keep
/// their structure, including empty folders. Symbolic links inside folders are
/// skipped.
pub fn build_manifest(paths: &[PathBuf]) -> Result<Manifest> {
    let mut manifest = Manifest {
        items: Vec::new(),
        sources: Vec::new(),
    };
    let mut names = HashSet::new();
    for path in paths {
        let path = std::fs::canonicalize(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("{} has no usable name", path.display()))?
            .to_string();
        if !names.insert(name.to_lowercase()) {
            bail!("two of the items to send are both named {name}");
        }
        add_entry(&mut manifest, &path, name)?;
    }
    if manifest.items.is_empty() {
        bail!("nothing to send");
    }
    Ok(manifest)
}

fn add_entry(manifest: &mut Manifest, path: &Path, rel_path: String) -> Result<()> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    if !meta.is_dir() {
        manifest.items.push(Item {
            rel_path,
            kind: ItemKind::File,
            size: meta.len(),
            mtime,
        });
        manifest.sources.push(path.to_path_buf());
        return Ok(());
    }

    manifest.items.push(Item {
        rel_path: rel_path.clone(),
        kind: ItemKind::Dir,
        size: 0,
        mtime,
    });
    manifest.sources.push(path.to_path_buf());

    let mut children = std::fs::read_dir(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|c| c.file_name());
    for child in children {
        let file_type = child.file_type()?;
        if !file_type.is_dir() && !file_type.is_file() {
            continue;
        }
        let name = child.file_name();
        let name = name
            .to_str()
            .with_context(|| format!("{} has no usable name", child.path().display()))?;
        add_entry(manifest, &child.path(), format!("{rel_path}/{name}"))?;
    }
    Ok(())
}

pub(crate) async fn send(
    shared: &Shared,
    endpoint: &Endpoint,
    to: EndpointId,
    paths: Vec<PathBuf>,
    remember: bool,
) -> Result<()> {
    let pending = PendingSend {
        to,
        paths: paths.clone(),
    };
    let manifest = match tokio::task::spawn_blocking(move || build_manifest(&paths)).await? {
        Ok(manifest) => Arc::new(manifest),
        Err(e) => {
            // The files are gone or unreadable: nothing to come back to.
            shared.outbox.remove(&pending)?;
            return Err(e);
        }
    };
    if remember {
        shared.outbox.add(pending.clone())?;
    }
    let id = transfer_id(&endpoint.id(), &manifest.items);
    let total_bytes = manifest.items.iter().map(|i| i.size).sum();
    let peer = shared
        .peers
        .get(&to)
        .map_or_else(|| to.fmt_short().to_string(), |p| p.name);
    shared.emit(Event::TransferStarted {
        id: id.clone(),
        peer,
        direction: Direction::Send,
        files: manifest
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::File)
            .count(),
        total_bytes,
        resumed_bytes: 0,
    });
    let progress = Arc::new(Progress::new(
        id.clone(),
        total_bytes,
        shared.events.clone(),
    ));

    let mut stalled = 0;
    let result = loop {
        let before = progress.done();
        let error = match attempt(endpoint, to, &manifest, &progress).await {
            Ok(()) => break Ok(()),
            Err(e) => e,
        };
        if error.is::<Fatal>() {
            break Err(error);
        }
        // Every attempt restarts `done` from what the receiver really holds,
        // so ending above the previous mark means the transfer moved forward.
        if progress.done() > before {
            stalled = 0;
        }
        stalled += 1;
        if stalled >= MAX_STALLED_ATTEMPTS {
            break Err(error.context("gave up after repeated failures"));
        }
        tracing::warn!("transfer interrupted, retrying: {error:#}");
        tokio::time::sleep(Duration::from_secs(1 << stalled.min(5))).await;
    };

    // Keep the entry only when giving up was about the connection, which a
    // later attempt may well get past.
    if result.as_ref().map_or_else(|e| e.is::<Fatal>(), |()| true) {
        shared.outbox.remove(&pending)?;
    }

    match &result {
        Ok(()) => shared.emit(Event::TransferFinished {
            id,
            direction: Direction::Send,
            saved: Vec::new(),
        }),
        Err(e) => shared.emit(Event::TransferFailed {
            id,
            direction: Direction::Send,
            error: format!("{e:#}"),
        }),
    }
    result
}

/// One connection's worth of trying: offer, send what is missing, finish.
async fn attempt(
    endpoint: &Endpoint,
    to: EndpointId,
    manifest: &Arc<Manifest>,
    progress: &Arc<Progress>,
) -> Result<()> {
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(to, ALPN_XFER))
        .await
        .map_err(|_| anyhow!("the other device is not reachable"))?
        .context("cannot connect to the other device")?;
    let (mut ctrl_send, mut ctrl_recv) = conn.open_bi().await?;
    let offer = Offer {
        items: manifest.items.clone(),
    };
    write_frame(&mut ctrl_send, &offer).await?;

    let offsets = match read_frame(&mut ctrl_recv).await? {
        Reply::Accept { offsets } => offsets,
        Reply::Reject { reason } => return Err(fatal(format!("refused: {reason}"))),
    };
    let valid = offsets.len() == manifest.items.len()
        && offsets
            .iter()
            .zip(&manifest.items)
            .all(|(o, i)| *o <= i.size);
    if !valid {
        return Err(fatal("the other device sent an invalid reply"));
    }
    progress.set(offsets.iter().sum());

    // Files the receiver does not hold completely yet.
    let todo: Vec<(usize, u64)> = manifest
        .items
        .iter()
        .zip(&offsets)
        .enumerate()
        .filter(|(_, (item, offset))| item.kind == ItemKind::File && **offset < item.size.max(1))
        .map(|(index, (_, offset))| (index, *offset))
        .collect();
    let todo = Arc::new(todo);
    let next = Arc::new(AtomicUsize::new(0));

    let mut workers = JoinSet::new();
    for _ in 0..PARALLEL_FILES.min(todo.len()) {
        let (conn, manifest, progress) = (conn.clone(), manifest.clone(), progress.clone());
        let (todo, next) = (todo.clone(), next.clone());
        workers.spawn(async move {
            while let Some(&(index, offset)) = todo.get(next.fetch_add(1, Ordering::Relaxed)) {
                send_file_with_retries(&conn, &manifest, &progress, index, offset).await?;
            }
            anyhow::Ok(())
        });
    }
    while let Some(done) = workers.join_next().await {
        done??;
    }

    write_frame(&mut ctrl_send, &Control::Finish).await?;
    match read_frame(&mut ctrl_recv).await? {
        Control::Finished => {}
        Control::Finish => bail!("unexpected control message"),
    }
    conn.close(0u32.into(), b"done");
    Ok(())
}

async fn send_file_with_retries(
    conn: &Connection,
    manifest: &Manifest,
    progress: &Progress,
    index: usize,
    mut offset: u64,
) -> Result<()> {
    let item = &manifest.items[index];
    for _ in 0..=MAX_FILE_RETRIES {
        let (mut send, mut recv) = conn.open_bi().await?;
        send_file(
            conn,
            &manifest.sources[index],
            item,
            index,
            offset,
            progress,
            &mut send,
        )
        .await?;
        send.finish()?;
        if read_status(&mut recv).await? == FILE_OK {
            return Ok(());
        }
        // The receiver threw the file away; send all of it again.
        progress.sub(item.size);
        offset = 0;
    }
    Err(fatal(format!("{} kept arriving corrupted", item.rel_path)))
}

async fn read_status<R: AsyncRead + Unpin>(recv: &mut R) -> Result<u8> {
    let mut status = [0u8; 1];
    recv.read_exact(&mut status)
        .await
        .context("connection lost")?;
    Ok(status[0])
}

async fn send_file<W: AsyncWrite + Unpin>(
    conn: &Connection,
    source: &Path,
    item: &Item,
    index: usize,
    offset: u64,
    progress: &Progress,
    send: &mut W,
) -> Result<()> {
    let header = FileHeader {
        index: index as u32,
        offset,
    };
    write_frame(send, &header).await?;

    // The trailing hash covers the whole file, so the part the receiver
    // already holds is hashed here too, without being sent.
    let mut hasher = hash_prefix(source.to_path_buf(), offset)
        .await
        .map_err(fatal)?;
    let mut file = fs::File::open(source)
        .await
        .map_err(|e| fatal(format!("cannot read {}: {e}", source.display())))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(fatal)?;

    let mut buf = vec![0u8; CHUNK];
    let mut left = item.size - offset;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        let n = file
            .read(&mut buf[..want])
            .await
            .map_err(|e| fatal(format!("cannot read {}: {e}", source.display())))?;
        if n == 0 {
            return Err(fatal(format!(
                "{} changed while it was being sent",
                source.display()
            )));
        }
        hasher.update(&buf[..n]);
        send.write_all(&buf[..n]).await.context("connection lost")?;
        left -= n as u64;
        progress.add(n as u64, conn);
    }
    send.write_all(hasher.finalize().as_bytes())
        .await
        .context("connection lost")?;
    Ok(())
}
