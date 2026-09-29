//! Tauri v2 mobile/desktop app entry point.
//!
//! Wraps the SvelteKit web frontend (`../../web/build`) into an Android APK.
//! This is a minimal shell: all product logic lives in the web frontend and
//! the axum server; the mobile crate only registers the plugins the frontend
//! needs (edge-to-edge insets, deep-link + opener for the OIDC flow).

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Edge-to-edge inset injection (status/navigation bar heights as CSS
        // variables) — auto-enables on webview load, no JS calls needed.
        .plugin(tauri_plugin_edge_to_edge::init())
        // OIDC login: open the OS browser (opener) and receive the device token
        // back via the `dev.aibuddy.app://auth` deep link. The frontend reads
        // the launch/opened URL through the plugin's JS API (getCurrent /
        // onOpenUrl); no Rust-side handler needed. Permissions:
        // "deep-link:default" / "opener:default" in capabilities/default.json.
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
