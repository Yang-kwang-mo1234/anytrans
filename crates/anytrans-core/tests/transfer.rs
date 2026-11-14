//! End-to-end tests: real nodes talking over loopback, with no relay and no
//! discovery service.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anytrans_core::{build_manifest, protocol::transfer_id, Direction, Event, Node, NodeOptions};
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

struct Device {
    node: Node,
    dir: TempDir,
}

impl Device {
    async fn start(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let node = Node::start(NodeOptions {
            data_dir: dir.path().join("data"),
            download_dir: dir.path().join("downloads"),
            device_name: Some(name.to_string()),
            mdns: false,
            local_only: true,
        })
        .await
        .unwrap();
        Self { node, dir }
    }

    fn downloads(&self) -> PathBuf {
        self.dir.path().join("downloads")
    }

    /// Creates a file to send, outside the download directory.
    fn source(&self, rel: &str, data: &[u8]) -> PathBuf {
        let path = self.dir.path().join("outbox").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }

    async fn send(&self, to: &Device, paths: Vec<PathBuf>) -> anyhow::Result<()> {
        tokio::time::timeout(
            Duration::from_secs(60),
            self.node.send(&to.node.id(), paths),
        )
        .await
        .expect("transfer timed out")
    }
}

async fn pair(host: &Device, joiner: &Device) {
    let ticket = host.node.pairing_ticket().await.unwrap();
    joiner.node.pair(&ticket).await.unwrap();
}

fn random_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|_| rand::random::<u8>()).collect()
}

fn drain(events: &mut Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

/// Relative path -> contents (`None` for a directory) of everything under `root`.
fn tree(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(dir: &Path, prefix: &str, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let rel = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                out.insert(rel.clone(), None);
                walk(&entry.path(), &format!("{rel}/"), out);
            } else {
                out.insert(rel, Some(fs::read(entry.path()).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, "", &mut out);
    out
}

#[tokio::test]
async fn sends_a_single_file() {
    let (mac, pc) = (Device::start("mac").await, Device::start("pc").await);
    pair(&mac, &pc).await;
    assert_eq!(mac.node.peers()[0].name, "pc");
    assert_eq!(pc.node.peers()[0].name, "mac");

    let data = random_bytes(3 * 1024 * 1024 + 17);
    let file = mac.source("photo.jpg", &data);
    let mut events = pc.node.subscribe();
    mac.send(&pc, vec![file]).await.unwrap();

    let saved = pc.downloads().join("photo.jpg");
    assert_eq!(fs::read(&saved).unwrap(), data);
    assert!(!pc.downloads().join(".anytrans-partial").exists());
    let finished = drain(&mut events).into_iter().any(|e| {
        matches!(e, Event::TransferFinished { direction: Direction::Receive, saved: s, .. } if s == [saved.clone()])
    });
    assert!(finished, "receiver should report where the file was saved");

    // The paired link works in the other direction too.
    let back = pc.source("reply.txt", b"got it");
    pc.send(&mac, vec![back]).await.unwrap();
    assert_eq!(
        fs::read(mac.downloads().join("reply.txt")).unwrap(),
        b"got it"
    );
}

#[tokio::test]
async fn sends_a_folder_with_its_structure() {
    let (mac, pc) = (Device::start("mac").await, Device::start("pc").await);
    pair(&mac, &pc).await;

    mac.source("album/a.txt", b"hello");
    mac.source("album/zero", b"");
    mac.source("album/sub/deep/b.bin", &random_bytes(1024 * 1024));
    mac.source("album/한글 이름.txt", "내용".as_bytes());
    let album = mac.dir.path().join("outbox").join("album");
    fs::create_dir(album.join("empty")).unwrap();
    let loose = mac.source("loose.txt", b"alone");

    mac.send(&pc, vec![album.clone(), loose.clone()])
        .await
        .unwrap();
    assert_eq!(tree(&pc.downloads().join("album")), tree(&album));
    assert!(pc.downloads().join("album").join("empty").is_dir());
    assert_eq!(
        fs::read(pc.downloads().join("loose.txt")).unwrap(),
        b"alone"
    );

    // Sending the same things again must not overwrite what is already there.
    mac.send(&pc, vec![album.clone(), loose]).await.unwrap();
    assert_eq!(tree(&pc.downloads().join("album (1)")), tree(&album));
    assert_eq!(
        fs::read(pc.downloads().join("loose (1).txt")).unwrap(),
        b"alone"
    );
}

/// Puts the first `prefix` into the receiver's staging area, as an interrupted
/// earlier attempt at sending `file` would have left it.
fn leave_partial(sender: &Device, receiver: &Device, file: &Path, prefix: &[u8]) {
    let manifest = build_manifest(&[file.to_path_buf()]).unwrap();
    let id = transfer_id(&sender.node.id(), &manifest.items);
    let staging = receiver.downloads().join(".anytrans-partial").join(id);
    fs::create_dir_all(&staging).unwrap();
    let name = format!("{}.anytrans-part", manifest.items[0].rel_path);
    fs::write(staging.join(name), prefix).unwrap();
}

#[tokio::test]
async fn resumes_an_interrupted_file() {
    let (mac, pc) = (Device::start("mac").await, Device::start("pc").await);
    pair(&mac, &pc).await;

    let data = random_bytes(5 * 1024 * 1024);
    let file = mac.source("movie.mp4", &data);
    let half = 2 * 1024 * 1024;
    leave_partial(&mac, &pc, &file, &data[..half]);

    let mut events = pc.node.subscribe();
    mac.send(&pc, vec![file]).await.unwrap();

    assert_eq!(fs::read(pc.downloads().join("movie.mp4")).unwrap(), data);
    let resumed = drain(&mut events).into_iter().find_map(|e| match e {
        Event::TransferStarted { resumed_bytes, .. } => Some(resumed_bytes),
        _ => None,
    });
    assert_eq!(
        resumed,
        Some(half as u64),
        "only the missing part should be sent"
    );
}

#[tokio::test]
async fn recovers_from_a_corrupted_partial() {
    let (mac, pc) = (Device::start("mac").await, Device::start("pc").await);
    pair(&mac, &pc).await;

    let data = random_bytes(2 * 1024 * 1024);
    let file = mac.source("doc.pdf", &data);
    leave_partial(&mac, &pc, &file, &vec![0xAA; 1024 * 1024]);

    mac.send(&pc, vec![file]).await.unwrap();
    assert_eq!(fs::read(pc.downloads().join("doc.pdf")).unwrap(), data);
}

#[tokio::test]
async fn refuses_devices_that_are_not_paired() {
    let (mac, stranger) = (Device::start("mac").await, Device::start("stranger").await);
    stranger.node.add_addr_hint(mac.node.addr());

    let file = stranger.source("malware.exe", b"boo");
    let error = stranger.send(&mac, vec![file]).await.unwrap_err();
    assert!(format!("{error:#}").contains("not paired"), "{error:#}");
    assert_eq!(fs::read_dir(mac.downloads()).unwrap().count(), 0);
}

#[tokio::test]
async fn pairing_codes_work_once() {
    let (mac, pc, third) = (
        Device::start("mac").await,
        Device::start("pc").await,
        Device::start("third").await,
    );
    assert!(pc.node.pair("not a code").await.is_err());

    let ticket = mac.node.pairing_ticket().await.unwrap();
    pc.node.pair(&ticket).await.unwrap();
    assert!(third.node.pair(&ticket).await.is_err());
    assert_eq!(mac.node.peers().len(), 1);
    assert!(third.node.peers().is_empty());
}
