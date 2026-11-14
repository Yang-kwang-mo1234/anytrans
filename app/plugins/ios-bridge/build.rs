// The plugin is called from the app's own Rust code only, so it exposes no
// commands to the web view.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).ios_path("ios").build();
}
