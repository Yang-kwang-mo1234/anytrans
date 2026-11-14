//! Receiving side of a transfer.
//!
//! Everything is written into a staging directory,
//! `<download>/.anytrans-partial/<transfer id>/`, laid out like the final
//! result. A file that is still arriving carries the [`PART_SUFFIX`]; it loses
//! the suffix once its hash checks out. When all files are in, the top-level
//! entries move into the download directory. An interrupted transfer leaves
//! the staging directory behind, and the next attempt picks up from it.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime},
};

use anyhow::{bail, ensure, Context, Result};
use iroh::endpoint::Connection;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
    task::JoinSet,
};

use crate::{
    events::{Direction, Event, Progress},
    fs_safe::{sanitize_rel_path, unique_dest},
    node::Shared,
    protocol::{
        hash_prefix, read_frame, transfer_id, write_frame, Control, FileHeader, Item, ItemKind,
        Offer, Reply, CHUNK, FILE_MISMATCH, FILE_OK,
    },
};

const PARTIAL_DIR: &str = ".anytrans-partial";
const PART_SUFFIX: &str = ".anytrans-part";
const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);

/// The connection currently receiving a given transfer, and a lock held while
/// it touches the staging directory.
pub(crate) struct Slot {
    conn: Connection,
    lock: Arc<tokio::sync::Mutex<()>>,
}

struct Transfer {
    id: String,
    items: Vec<Item>,
    /// Sanitized local relative path of each item.
    paths: Vec<PathBuf>,
    staging: PathBuf,
    /// Set while a stream is writing the item, so no two streams share a file.
    busy: Vec<AtomicBool>,
    progress: Progress,
}

pub(crate) async fn handle(shared: &Arc<Shared>, conn: Connection) -> Result<()> {
    let remote = conn.remote_id();
    let (mut ctrl_send, mut ctrl_recv) = conn.accept_bi().await?;
    let offer: Offer = read_frame(&mut ctrl_recv).await?;

    let Some(peer) = shared.peers.get(&remote) else {
        return reject(
            &conn,
            &mut ctrl_send,
            "this device is not paired with the sender",
        )
        .await;
    };
    let paths = match local_paths(&offer.items) {
        Ok(paths) => paths,
        Err(e) => return reject(&conn, &mut ctrl_send, &format!("{e:#}")).await,
    };
    let id = transfer_id(&remote, &offer.items);

    // A retry can arrive while the connection of the previous attempt has not
    // noticed it is dead yet. Close that one and wait for it to let go.
    let lock = {
        let mut receiving = shared.receiving.lock().unwrap();
        let lock = match receiving.remove(&id) {
            Some(old) => {
                old.conn.close(1u32.into(), b"superseded");
                old.lock
            }
            None => Arc::default(),
        };
        receiving.insert(
            id.clone(),
            Slot {
                conn: conn.clone(),
                lock: lock.clone(),
            },
        );
        lock
    };
    let guard = lock.lock().await;

    let total_bytes = offer.items.iter().map(|i| i.size).sum();
    let transfer = Arc::new(Transfer {
        staging: shared.download_dir.join(PARTIAL_DIR).join(&id),
        busy: offer.items.iter().map(|_| AtomicBool::new(false)).collect(),
        progress: Progress::new(id.clone(), total_bytes, shared.events.clone()),
        id: id.clone(),
        items: offer.items,
        paths,
    });

    let result = receive(
        shared,
        &peer.name,
        &conn,
        &mut ctrl_send,
        ctrl_recv,
        &transfer,
    )
    .await;

    drop(guard);
    {
        let mut receiving = shared.receiving.lock().unwrap();
        if receiving
            .get(&id)
            .is_some_and(|slot| slot.conn.stable_id() == conn.stable_id())
        {
            receiving.remove(&id);
        }
    }

    match result {
        Ok(saved) => {
            // The files are in place whether or not the sender hears about it,
            // so report success before telling the sender.
            shared.emit(Event::TransferFinished {
                id,
                direction: Direction::Receive,
                saved,
            });
            write_frame(&mut ctrl_send, &Control::Finished).await?;
            ctrl_send.finish()?;
            let _ = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
            Ok(())
        }
        Err(e) => {
            conn.close(1u32.into(), b"receive failed");
            shared.emit(Event::TransferFailed {
                id,
                direction: Direction::Receive,
                error: format!("{e:#}"),
            });
            Err(e)
        }
    }
}

async fn reject(
    conn: &Connection,
    ctrl_send: &mut iroh::endpoint::SendStream,
    reason: &str,
) -> Result<()> {
    let reply = Reply::Reject {
        reason: reason.to_string(),
    };
    write_frame(ctrl_send, &reply).await?;
    ctrl_send.finish()?;
    let _ = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
    Ok(())
}

/// Maps every offered path to a safe local one, refusing offers in which two
/// items would land on the same file.
fn local_paths(items: &[Item]) -> Result<Vec<PathBuf>> {
    let mut seen = HashSet::new();
    let mut paths = Vec::with_capacity(items.len());
    for item in items {
        let path = sanitize_rel_path(&item.rel_path)?;
        // Lower-cased because macOS and Windows file systems ignore case.
        if !seen.insert(path.to_string_lossy().to_lowercase()) {
            bail!("two items would be saved as {}", path.display());
        }
        paths.push(path);
    }
    Ok(paths)
}

async fn receive(
    shared: &Shared,
    peer_name: &str,
    conn: &Connection,
    ctrl_send: &mut iroh::endpoint::SendStream,
    mut ctrl_recv: iroh::endpoint::RecvStream,
    transfer: &Arc<Transfer>,
) -> Result<Vec<PathBuf>> {
    let offsets = prepare_staging(transfer).await?;
    let resumed_bytes: u64 = offsets.iter().sum();
    transfer.progress.set(resumed_bytes);
    shared.emit(Event::TransferStarted {
        id: transfer.id.clone(),
        peer: peer_name.to_string(),
        direction: Direction::Receive,
        files: transfer
            .items
            .iter()
            .filter(|i| i.kind == ItemKind::File)
            .count(),
        total_bytes: transfer.items.iter().map(|i| i.size).sum(),
        resumed_bytes,
    });
    write_frame(ctrl_send, &Reply::Accept { offsets }).await?;

    let mut tasks = JoinSet::new();
    let accepted = accept_files(conn, &mut ctrl_recv, transfer, &mut tasks).await;
    // No file task may outlive this function: the next attempt relies on it.
    tasks.shutdown().await;
    accepted?;

    finalize(&shared.download_dir, transfer).await
}

/// Creates the staging tree and reports, per item, how much is already there.
async fn prepare_staging(transfer: &Transfer) -> Result<Vec<u64>> {
    fs::create_dir_all(&transfer.staging).await?;
    let mut offsets = Vec::with_capacity(transfer.items.len());
    for (item, rel) in transfer.items.iter().zip(&transfer.paths) {
        let path = transfer.staging.join(rel);
        if item.kind == ItemKind::Dir {
            fs::create_dir_all(&path).await?;
            offsets.push(0);
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        if fs::metadata(&path)
            .await
            .is_ok_and(|m| m.len() == item.size)
        {
            offsets.push(item.size);
            continue;
        }
        let part = part_path(&path);
        let offset = match fs::metadata(&part).await {
            // A complete but unverified part is not worth trusting.
            Ok(m) if m.len() < item.size => m.len(),
            Ok(_) => {
                fs::remove_file(&part).await?;
                0
            }
            Err(_) => 0,
        };
        offsets.push(offset);
    }
    Ok(offsets)
}

fn part_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(PART_SUFFIX);
    PathBuf::from(os)
}

/// Accepts one stream per file until the sender says it is finished.
async fn accept_files(
    conn: &Connection,
    ctrl_recv: &mut iroh::endpoint::RecvStream,
    transfer: &Arc<Transfer>,
    tasks: &mut JoinSet<Result<()>>,
) -> Result<()> {
    let finish = read_frame::<_, Control>(ctrl_recv);
    tokio::pin!(finish);
    loop {
        tokio::select! {
            msg = &mut finish => {
                ensure!(matches!(msg?, Control::Finish), "unexpected control message");
                break;
            }
            stream = conn.accept_bi() => {
                let (mut send, mut recv) = stream.context("connection lost")?;
                let transfer = transfer.clone();
                let conn = conn.clone();
                tasks.spawn(async move {
                    receive_file(&transfer, &conn, &mut send, &mut recv).await?;
                    send.finish()?;
                    Ok(())
                });
            }
            Some(done) = tasks.join_next() => done??,
        }
    }
    while let Some(done) = tasks.join_next().await {
        done??;
    }
    Ok(())
}

async fn receive_file<W, R>(
    transfer: &Transfer,
    conn: &Connection,
    send: &mut W,
    recv: &mut R,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let header: FileHeader = read_frame(recv).await?;
    let index = header.index as usize;
    let item = transfer.items.get(index).context("unknown file index")?;
    ensure!(item.kind == ItemKind::File, "item {index} is not a file");
    ensure!(header.offset <= item.size, "offset beyond end of file");
    ensure!(
        !transfer.busy[index].swap(true, Ordering::SeqCst),
        "file {index} is already being received"
    );

    let path = transfer.staging.join(&transfer.paths[index]);
    let part = part_path(&path);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&part)
        .await
        .with_context(|| format!("cannot write {}", part.display()))?;
    let existing = file.metadata().await?.len();
    ensure!(
        header.offset == 0 || header.offset == existing,
        "sender resumed at {} but {existing} bytes are stored",
        header.offset
    );
    file.set_len(header.offset).await?;
    file.seek(std::io::SeekFrom::End(0)).await?;

    let mut hasher = hash_prefix(part.clone(), header.offset).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut left = item.size - header.offset;
    while left > 0 {
        let want = left.min(CHUNK as u64) as usize;
        let n = recv
            .read(&mut buf[..want])
            .await
            .context("connection lost")?;
        ensure!(n > 0, "connection lost");
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .await
            .with_context(|| format!("cannot write {}", part.display()))?;
        left -= n as u64;
        transfer.progress.add(n as u64, conn);
    }
    file.flush().await?;

    let mut expected = [0u8; 32];
    recv.read_exact(&mut expected)
        .await
        .context("connection lost")?;

    let status = if hasher.finalize() == blake3::Hash::from_bytes(expected) {
        let file = file.into_std().await;
        if item.mtime > 0 {
            let _ = file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(item.mtime));
        }
        drop(file);
        fs::rename(&part, &path).await?;
        FILE_OK
    } else {
        drop(file);
        fs::remove_file(&part).await?;
        transfer.progress.sub(item.size);
        FILE_MISMATCH
    };
    transfer.busy[index].store(false, Ordering::SeqCst);
    send.write_all(&[status]).await?;
    Ok(())
}

/// Moves the finished staging tree into the download directory.
async fn finalize(download_dir: &Path, transfer: &Transfer) -> Result<Vec<PathBuf>> {
    for (item, rel) in transfer.items.iter().zip(&transfer.paths) {
        if item.kind == ItemKind::File {
            let stored = fs::metadata(transfer.staging.join(rel)).await;
            ensure!(
                stored.is_ok_and(|m| m.len() == item.size),
                "{} did not arrive completely",
                rel.display()
            );
        }
    }

    let mut saved = Vec::new();
    let mut moved = HashSet::new();
    for (item, rel) in transfer.items.iter().zip(&transfer.paths) {
        let Some(top) = rel.iter().next().and_then(|c| c.to_str()) else {
            continue;
        };
        if !moved.insert(top.to_string()) {
            continue;
        }
        let is_dir = item.kind == ItemKind::Dir || rel.iter().count() > 1;
        let dest = unique_dest(download_dir, top, is_dir);
        fs::rename(transfer.staging.join(top), &dest)
            .await
            .with_context(|| format!("cannot move {top} into place"))?;
        saved.push(dest);
    }
    let _ = fs::remove_dir_all(&transfer.staging).await;
    // Succeeds only when no other transfer is in progress, which is the intent.
    let _ = fs::remove_dir(download_dir.join(PARTIAL_DIR)).await;
    Ok(saved)
}

/// Deletes staging directories of transfers nobody came back to finish.
pub(crate) fn remove_stale_partials(download_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(download_dir.join(PARTIAL_DIR)) else {
        return;
    };
    for entry in entries.flatten() {
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        if age.is_some_and(|age| age > STALE_AFTER) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}
