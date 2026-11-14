//! The app shell: starts an [`anytrans_core::Node`], exposes it to the UI as
//! commands and forwards its events. All transfer logic lives in the core.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anytrans_core::{EndpointId, Node, NodeOptions, Peer, PendingSend};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::broadcast::error::RecvError;

#[cfg(target_os = "ios")]
use tauri_plugin_anytrans_bridge::BridgeExt;

/// What the user can change on the settings screen, stored as
/// `settings.json` next to the device key.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    /// Name other devices see. `None` keeps the current one.
    device_name: Option<String>,
    /// Where received files go (desktop only). `None` is the default folder.
    download_dir: Option<PathBuf>,
    /// Relay server to use instead of the public ones.
    relay_url: Option<String>,
    /// On a metered connection, ask before sending more than this.
    cellular_limit_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: None,
            download_dir: None,
            relay_url: None,
            cellular_limit_mb: 200,
        }
    }
}

impl Settings {
    fn load(data_dir: &Path) -> Self {
        std::fs::read(data_dir.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, data_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(data_dir).map_err(message)?;
        let json = serde_json::to_vec_pretty(self).map_err(message)?;
        std::fs::write(data_dir.join("settings.json"), json).map_err(message)
    }

    /// Blank text fields mean "not set".
    fn normalized(mut self) -> Self {
        let blank = |s: &String| s.trim().is_empty();
        self.device_name = self.device_name.filter(|s| !blank(s));
        self.relay_url = self.relay_url.filter(|s| !blank(s));
        self.download_dir = self.download_dir.filter(|p| !p.as_os_str().is_empty());
        self
    }
}

struct AppState {
    /// The running node, or why it could not start. The window opens either
    /// way so the reason can be shown.
    node: Result<Node, String>,
    data_dir: PathBuf,
    settings: Mutex<Settings>,
}

impl AppState {
    fn node(&self) -> Result<&Node, String> {
        self.node.as_ref().map_err(Clone::clone)
    }
}

fn message(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

#[derive(Serialize)]
struct Info {
    id: String,
    name: String,
    download_dir: PathBuf,
    /// Phone or tablet: no folder picking, no drag and drop, but a camera.
    mobile: bool,
}

#[tauri::command]
fn info(state: State<'_, AppState>) -> Result<Info, String> {
    let node = state.node()?;
    Ok(Info {
        id: node.id().to_string(),
        name: node.name().to_string(),
        download_dir: node.download_dir().to_path_buf(),
        mobile: cfg!(mobile),
    })
}

#[tauri::command]
fn peers(state: State<'_, AppState>) -> Result<Vec<Peer>, String> {
    Ok(state.node()?.peers())
}

#[derive(Serialize)]
struct Ticket {
    code: String,
    /// The code as a QR image (SVG markup) for a phone to scan.
    qr: String,
}

#[tauri::command]
async fn pairing_ticket(state: State<'_, AppState>) -> Result<Ticket, String> {
    let code = state.node()?.pairing_ticket().await.map_err(message)?;
    // The code is upper-case base32, which QR codes store compactly.
    let qr = qrcode::QrCode::new(code.as_bytes())
        .map_err(message)?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .build();
    Ok(Ticket { code, qr })
}

#[tauri::command]
async fn pair(state: State<'_, AppState>, code: String) -> Result<Peer, String> {
    state.node()?.pair(&code).await.map_err(message)
}

#[tauri::command]
fn forget(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id: EndpointId = id.parse().map_err(message)?;
    state.node()?.forget_peer(&id).map_err(message)?;
    Ok(())
}

/// File pickers hand back plain paths on desktop and `file://` URLs on iOS.
fn local_path(picked: String) -> Result<PathBuf, String> {
    if !picked.starts_with("file://") {
        return Ok(PathBuf::from(picked));
    }
    tauri::Url::parse(&picked)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .ok_or_else(|| format!("cannot read {picked}"))
}

fn local_paths(picked: Vec<String>) -> Result<Vec<PathBuf>, String> {
    picked.into_iter().map(local_path).collect()
}

/// Resolves once the transfer has finished; progress arrives as events.
#[tauri::command]
async fn send(
    app: AppHandle,
    state: State<'_, AppState>,
    to: String,
    paths: Vec<String>,
) -> Result<(), String> {
    let to: EndpointId = to.parse().map_err(message)?;
    send_remembered(&app, state.node()?, to, local_paths(paths)?).await
}

/// Sends, and afterwards clears away the copies iOS made for us, unless the
/// transfer is still waiting to be finished later.
async fn send_remembered(
    app: &AppHandle,
    node: &Node,
    to: EndpointId,
    paths: Vec<PathBuf>,
) -> Result<(), String> {
    let result = node.send_and_remember(&to, paths.clone()).await;
    let waiting = node.pending_sends().iter().any(|p| p.paths == paths);
    if cfg!(mobile) && !waiting {
        remove_app_copies(app, &paths);
    }
    result.map_err(message)
}

/// Continues what was left unfinished when the app last closed.
fn resume_pending(app: &AppHandle, pending: Vec<PendingSend>) {
    for PendingSend { to, paths } in pending {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(node) = app.state::<AppState>().node() {
                // The outcome reaches the UI as events, like any transfer.
                let _ = send_remembered(&app, node, to, paths).await;
            }
        });
    }
}

/// Total size in bytes of what `paths` would send.
#[tauri::command]
async fn total_size(paths: Vec<String>) -> Result<u64, String> {
    let paths = local_paths(paths)?;
    let manifest = tauri::async_runtime::spawn_blocking(move || {
        anytrans_core::build_manifest(&paths).map_err(message)
    })
    .await
    .map_err(message)??;
    Ok(manifest.items.iter().map(|item| item.size).sum())
}

/// The folder the share extension fills, on iOS.
fn shared_inbox(_app: &AppHandle) -> Option<PathBuf> {
    #[cfg(target_os = "ios")]
    return _app.bridge().shared_inbox();
    #[cfg(not(target_os = "ios"))]
    None
}

/// Deletes files that exist only because iOS copied them for this app: picker
/// results in the temp folder and items from the share extension.
///
/// Never call this on desktop, where a path under the temp folder is the
/// user's own file.
fn remove_app_copies(app: &AppHandle, paths: &[PathBuf]) {
    let roots: Vec<PathBuf> = [Some(std::env::temp_dir()), shared_inbox(app)]
        .into_iter()
        .flatten()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    for path in paths {
        let Ok(real) = path.canonicalize() else {
            continue;
        };
        if roots.iter().any(|root| real.starts_with(root)) {
            let _ = std::fs::remove_file(&real);
            // Share batches each sit in their own folder; drop it once empty.
            if let Some(parent) = real.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

/// What the share extension has left for the app to send, if anything.
#[tauri::command]
fn shared_items(app: AppHandle) -> Vec<PathBuf> {
    let Some(inbox) = shared_inbox(&app) else {
        return Vec::new();
    };
    let files_in = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    let mut items: Vec<PathBuf> = files_in(&inbox)
        .iter()
        .flat_map(|batch| files_in(batch))
        .collect();
    items.sort();
    items
}

/// Throws away shared or picked items the user decided not to send.
#[tauri::command]
fn discard_shared(app: AppHandle, paths: Vec<String>) {
    if cfg!(mobile) {
        if let Ok(paths) = local_paths(paths) {
            remove_app_copies(&app, &paths);
        }
    }
}

/// Adds received images and videos to the photo library. Returns how many
/// were added.
#[tauri::command]
async fn save_to_photos(_app: AppHandle, paths: Vec<PathBuf>) -> Result<usize, String> {
    #[cfg(target_os = "ios")]
    {
        tauri::async_runtime::spawn_blocking(move || {
            for path in &paths {
                _app.bridge().save_to_photos(&path.to_string_lossy())?;
            }
            Ok(paths.len())
        })
        .await
        .map_err(message)?
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = paths;
        Err("only available on iPhone and iPad".to_string())
    }
}

#[derive(Serialize)]
struct NetworkStatus {
    /// The connection is metered: cellular or a personal hotspot.
    expensive: bool,
}

#[tauri::command]
fn network_status(_app: AppHandle) -> NetworkStatus {
    #[cfg(target_os = "ios")]
    let expensive = _app.bridge().network().is_some_and(|n| n.expensive);
    #[cfg(not(target_os = "ios"))]
    let expensive = false;
    NetworkStatus { expensive }
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

/// Stores the settings. The name, download folder and relay take effect the
/// next time the app starts.
#[tauri::command]
fn save_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    let settings = settings.normalized();
    settings.save(&state.data_dir)?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
}

/// Shows the received files: the folder on desktop, the Files app on iOS.
#[tauri::command]
fn open_downloads(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.node()?.download_dir().to_string_lossy().into_owned();
    if cfg!(target_os = "ios") {
        let url = format!("shareddocuments://{dir}");
        app.opener().open_url(url, None::<&str>).map_err(message)
    } else {
        app.opener().open_path(dir, None::<&str>).map_err(message)
    }
}

/// Where the device key, paired devices and settings live.
///
/// On desktop this is the directory the CLI uses too, so both are the same
/// device; `ANYTRANS_DATA_DIR` overrides it.
fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if cfg!(mobile) {
        return app.path().app_data_dir().map_err(message);
    }
    match std::env::var_os("ANYTRANS_DATA_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(app.path().config_dir().map_err(message)?.join("anytrans")),
    }
}

/// Where received files go. On mobile that is the app's Documents folder,
/// which the Files app shows. On desktop the setting wins, then
/// `ANYTRANS_DOWNLOAD_DIR`, then `Downloads/AnyTrans`.
fn download_dir(app: &AppHandle, settings: &Settings) -> Result<PathBuf, String> {
    if cfg!(mobile) {
        return app.path().document_dir().map_err(message);
    }
    if let Some(dir) = &settings.download_dir {
        return Ok(dir.clone());
    }
    match std::env::var_os("ANYTRANS_DOWNLOAD_DIR") {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => Ok(app.path().download_dir().map_err(message)?.join("AnyTrans")),
    }
}

async fn start_node(app: &AppHandle, data_dir: &Path, settings: &Settings) -> Result<Node, String> {
    Node::start(NodeOptions {
        data_dir: data_dir.to_path_buf(),
        download_dir: download_dir(app, settings)?,
        device_name: settings.device_name.clone(),
        // iOS only lets apps holding a special entitlement send the multicast
        // packets mDNS needs. Devices there are found through the relay.
        mdns: cfg!(desktop),
        relay_url: settings.relay_url.clone(),
        local_only: false,
    })
    .await
    .map_err(message)
}

fn forward_events(app: AppHandle, node: &Node) {
    let mut events = node.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app.emit("anytrans-event", event);
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(desktop)]
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The tray icon keeps the app reachable while its window is closed, which is
/// when it is still receiving.
#[cfg(desktop)]
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::{
        menu::{Menu, MenuItem},
        tray::TrayIconBuilder,
    };

    let open = MenuItem::with_id(app, "open", "AnyTrans 열기", true, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "받은 파일 폴더", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &folder, &quit])?;

    let mut tray = TrayIconBuilder::new()
        .tooltip("AnyTrans")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "folder" => {
                let _ = open_downloads(app.clone(), app.state());
            }
            "quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

/// Passed by the login item so the app starts in the tray without a window.
#[cfg(desktop)]
const HIDDEN_FLAG: &str = "--hidden";

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let (data_dir, settings, node) = match data_dir(&handle) {
        Ok(dir) => {
            let settings = Settings::load(&dir);
            let node = tauri::async_runtime::block_on(start_node(&handle, &dir, &settings));
            (dir, settings, node)
        }
        Err(e) => (PathBuf::new(), Settings::default(), Err(e)),
    };

    let pending = match &node {
        Ok(node) => {
            forward_events(handle.clone(), node);
            node.pending_sends()
        }
        Err(_) => Vec::new(),
    };
    app.manage(AppState {
        node,
        data_dir,
        settings: Mutex::new(settings),
    });
    resume_pending(&handle, pending);

    #[cfg(desktop)]
    {
        build_tray(app)?;
        if std::env::args().any(|arg| arg == HIDDEN_FLAG) {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init());

    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    #[cfg(target_os = "ios")]
    let builder = builder.plugin(tauri_plugin_anytrans_bridge::init());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec![HIDDEN_FLAG]),
    ));

    let builder = builder
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            info,
            peers,
            pairing_ticket,
            pair,
            forget,
            send,
            total_size,
            shared_items,
            discard_shared,
            save_to_photos,
            network_status,
            get_settings,
            save_settings,
            restart_app,
            open_downloads
        ]);

    // Closing the window only hides it: the app keeps receiving from the tray.
    #[cfg(desktop)]
    let builder = builder.on_window_event(|window, event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            let _ = window.hide();
            api.prevent_close();
        }
    });

    let app = builder
        .build(tauri::generate_context!())
        .expect("failed to start AnyTrans");
    app.run(|_app, _event| {
        // Clicking the Dock icon brings the hidden window back.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Reopen { .. } = _event {
            show_main_window(_app);
        }
        // The share extension opens `anytrans://share` once it has left
        // something in the shared inbox.
        #[cfg(target_os = "ios")]
        if let tauri::RunEvent::Opened { .. } = _event {
            let _ = _app.emit("anytrans-shared", ());
        }
    });
}
