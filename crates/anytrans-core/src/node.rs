//! A running device: the iroh endpoint plus everything the protocols share.

use std::{
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use iroh::{
    address_lookup::MemoryLookup,
    endpoint::{presets, Connection},
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr, EndpointId, RelayMode,
};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use tokio::sync::broadcast;

use crate::{
    events::Event,
    identity, pairing,
    peers::{Peer, PeerStore},
    protocol::{ALPN_PAIR, ALPN_XFER},
    recv, send,
};

pub struct NodeOptions {
    /// Where the device key and the list of paired devices live.
    pub data_dir: PathBuf,
    /// Where received files are saved.
    pub download_dir: PathBuf,
    /// Name shown to other devices. `None` keeps the stored name, or falls back
    /// to the host name on first start.
    pub device_name: Option<String>,
    /// Use no relay and no discovery service: peers are reachable only at
    /// addresses learned while pairing. Meant for tests.
    pub local_only: bool,
}

/// State shared by the node and its protocol handlers.
pub(crate) struct Shared {
    pub(crate) name: String,
    pub(crate) download_dir: PathBuf,
    pub(crate) peers: PeerStore,
    pub(crate) events: broadcast::Sender<Event>,
    pub(crate) pending_pair: Mutex<Option<pairing::Pending>>,
    pub(crate) lookup: MemoryLookup,
    /// Transfers currently being received, by transfer id.
    pub(crate) receiving: Mutex<HashMap<String, recv::Slot>>,
}

impl Shared {
    pub(crate) fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

pub struct Node {
    router: Router,
    shared: Arc<Shared>,
    local_only: bool,
    /// Held for the node's lifetime so two processes never share one identity.
    _lock: fs::File,
}

impl Node {
    pub async fn start(opts: NodeOptions) -> Result<Self> {
        fs::create_dir_all(&opts.data_dir)
            .with_context(|| format!("cannot create {}", opts.data_dir.display()))?;
        fs::create_dir_all(&opts.download_dir)
            .with_context(|| format!("cannot create {}", opts.download_dir.display()))?;

        let lock = fs::File::create(opts.data_dir.join("lock"))?;
        if lock.try_lock().is_err() {
            bail!("AnyTrans is already running with this data directory");
        }

        let key = identity::load_or_create(&opts.data_dir)?;
        let name = device_name(&opts.data_dir, opts.device_name)?;
        let lookup = MemoryLookup::new();

        let builder = if opts.local_only {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        } else {
            Endpoint::builder(presets::N0).address_lookup(MdnsAddressLookup::builder())
        };
        let endpoint = builder
            .secret_key(key)
            .address_lookup(lookup.clone())
            .bind()
            .await
            .context("cannot start networking")?;

        let (events, _) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            name,
            download_dir: opts.download_dir,
            peers: PeerStore::load(&opts.data_dir)?,
            events,
            pending_pair: Mutex::new(None),
            lookup,
            receiving: Mutex::new(HashMap::new()),
        });
        recv::remove_stale_partials(&shared.download_dir);

        let router = Router::builder(endpoint)
            .accept(ALPN_PAIR, PairHandler(shared.clone()))
            .accept(ALPN_XFER, XferHandler(shared.clone()))
            .spawn();

        Ok(Self {
            router,
            shared,
            local_only: opts.local_only,
            _lock: lock,
        })
    }

    pub fn id(&self) -> EndpointId {
        self.router.endpoint().id()
    }

    pub fn name(&self) -> &str {
        &self.shared.name
    }

    pub fn download_dir(&self) -> &Path {
        &self.shared.download_dir
    }

    /// The addresses this device can currently be reached at.
    pub fn addr(&self) -> EndpointAddr {
        self.router.endpoint().addr()
    }

    /// Tells this node where another device can be reached, without waiting
    /// for discovery to find it.
    pub fn add_addr_hint(&self, addr: EndpointAddr) {
        self.shared.lookup.add_endpoint_info(addr);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.shared.events.subscribe()
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.shared.peers.list()
    }

    /// Finds a paired device by name or by id prefix.
    pub fn find_peer(&self, query: &str) -> Result<Peer> {
        self.shared.peers.find(query)
    }

    pub fn forget_peer(&self, id: &EndpointId) -> Result<bool> {
        self.shared.peers.remove(id)
    }

    /// Creates a one-time pairing code to show on this device. It replaces any
    /// earlier code and expires after [`pairing::TICKET_TTL`].
    pub async fn pairing_ticket(&self) -> Result<String> {
        if !self.local_only {
            // Give the relay a moment to connect so the code works off-LAN too.
            let _ =
                tokio::time::timeout(Duration::from_secs(5), self.router.endpoint().online()).await;
        }
        pairing::create_ticket(&self.shared, self.addr())
    }

    /// Pairs with the device that issued `ticket`.
    pub async fn pair(&self, ticket: &str) -> Result<Peer> {
        pairing::join(&self.shared, self.router.endpoint(), ticket).await
    }

    /// Sends files and folders to a device, resuming whatever part of the same
    /// transfer the device already holds. Returns once everything has arrived.
    pub async fn send(&self, to: &EndpointId, paths: Vec<PathBuf>) -> Result<()> {
        send::send(&self.shared, self.router.endpoint(), *to, paths).await
    }

    pub async fn shutdown(self) {
        let _ = self.router.shutdown().await;
    }
}

fn device_name(data_dir: &Path, requested: Option<String>) -> Result<String> {
    let path = data_dir.join("name");
    if let Some(name) = requested {
        fs::write(&path, &name)?;
        return Ok(name);
    }
    if let Ok(name) = fs::read_to_string(&path) {
        let name = name.trim();
        if !name.is_empty() {
            return Ok(name.to_string());
        }
    }
    Ok(gethostname::gethostname().to_string_lossy().into_owned())
}

struct PairHandler(Arc<Shared>);
struct XferHandler(Arc<Shared>);

impl fmt::Debug for PairHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairHandler")
    }
}

impl fmt::Debug for XferHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("XferHandler")
    }
}

impl ProtocolHandler for PairHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        pairing::handle(&self.0, conn)
            .await
            .map_err(|e| AcceptError::from_boxed(e.into()))
    }
}

impl ProtocolHandler for XferHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        recv::handle(&self.0, conn)
            .await
            .map_err(|e| AcceptError::from_boxed(e.into()))
    }
}
