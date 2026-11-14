//! The app shell: starts an [`anytrans_core::Node`], exposes it to the UI as
//! commands and forwards its events. All transfer logic lives in the core.

use std::path::PathBuf;

use anytrans_core::{EndpointId, Node, NodeOptions, Peer};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::broadcast::error::RecvError;

/// The running node, or why it could not start. The window opens either way
/// so the reason can be shown.
struct AppState(Result<Node, String>);

impl AppState {
    fn node(&self) -> Result<&Node, String> {
        self.0.as_ref().map_err(Clone::clone)
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
}

#[tauri::command]
fn info(state: State<'_, AppState>) -> Result<Info, String> {
    let node = state.node()?;
    Ok(Info {
        id: node.id().to_string(),
        name: node.name().to_string(),
        download_dir: node.download_dir().to_path_buf(),
    })
}

#[tauri::command]
fn peers(state: State<'_, AppState>) -> Result<Vec<Peer>, String> {
    Ok(state.node()?.peers())
}

#[tauri::command]
async fn pairing_ticket(state: State<'_, AppState>) -> Result<String, String> {
    state.node()?.pairing_ticket().await.map_err(message)
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

/// Resolves once the transfer has finished; progress arrives as events.
#[tauri::command]
async fn send(state: State<'_, AppState>, to: String, paths: Vec<PathBuf>) -> Result<(), String> {
    let to: EndpointId = to.parse().map_err(message)?;
    state.node()?.send(&to, paths).await.map_err(message)
}

#[tauri::command]
fn open_downloads(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.node()?.download_dir().to_string_lossy().into_owned();
    app.opener().open_path(dir, None::<&str>).map_err(message)
}

/// Starts the node. `ANYTRANS_DATA_DIR` and `ANYTRANS_DOWNLOAD_DIR` override
/// the default locations, which are the same ones the CLI uses.
async fn start_node(app: &AppHandle) -> Result<Node, String> {
    let dir = |var: &str| std::env::var_os(var).map(PathBuf::from);
    let data_dir = match dir("ANYTRANS_DATA_DIR") {
        Some(dir) => dir,
        None => app.path().config_dir().map_err(message)?.join("anytrans"),
    };
    let download_dir = match dir("ANYTRANS_DOWNLOAD_DIR") {
        Some(dir) => dir,
        None => app.path().download_dir().map_err(message)?.join("AnyTrans"),
    };
    Node::start(NodeOptions {
        data_dir,
        download_dir,
        device_name: None,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let node = tauri::async_runtime::block_on(start_node(app.handle()));
            if let Ok(node) = &node {
                forward_events(app.handle().clone(), node);
            }
            app.manage(AppState(node));
            #[cfg(desktop)]
            build_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            info,
            peers,
            pairing_ticket,
            pair,
            forget,
            send,
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
    });
}
