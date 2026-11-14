//! The parts of AnyTrans that need native iOS code: finding what the share
//! extension left for the app, saving into the photo library, and telling
//! whether the current connection is a metered one.
//!
//! On every other platform this crate is empty.

#![cfg(target_os = "ios")]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Manager, Runtime,
};

tauri::ios_plugin_binding!(init_plugin_anytrans_bridge);

pub struct Bridge<R: Runtime>(PluginHandle<R>);

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Network {
    /// The system considers the connection costly: cellular or a hotspot.
    pub expensive: bool,
}

#[derive(Deserialize)]
struct InboxResponse {
    dir: Option<PathBuf>,
}

#[derive(Serialize)]
struct SaveRequest<'a> {
    path: &'a str,
}

impl<R: Runtime> Bridge<R> {
    /// The folder the share extension copies shared items into, if the app
    /// group is available.
    pub fn shared_inbox(&self) -> Option<PathBuf> {
        self.0
            .run_mobile_plugin::<InboxResponse>("sharedInbox", ())
            .ok()
            .and_then(|response| response.dir)
    }

    /// Adds an image or video file to the photo library.
    pub fn save_to_photos(&self, path: &str) -> Result<(), String> {
        self.0
            .run_mobile_plugin::<()>("saveToPhotos", SaveRequest { path })
            .map_err(|e| e.to_string())
    }

    pub fn network(&self) -> Option<Network> {
        self.0.run_mobile_plugin::<Network>("network", ()).ok()
    }
}

pub trait BridgeExt<R: Runtime> {
    fn bridge(&self) -> &Bridge<R>;
}

impl<R: Runtime, T: Manager<R>> BridgeExt<R> for T {
    fn bridge(&self) -> &Bridge<R> {
        self.state::<Bridge<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("anytrans-bridge")
        .setup(|app, api| {
            let handle = api.register_ios_plugin(init_plugin_anytrans_bridge)?;
            app.manage(Bridge(handle));
            Ok(())
        })
        .build()
}
