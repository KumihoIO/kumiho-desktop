// Bringing Desktop back to the front.
//
// macOS keeps the process alive after its last window closes, so `main` can be
// gone while Desktop is still running. Focusing a window that no longer exists
// is a no-op, and `single_instance` makes every later launch exit cleanly
// against that live process, so the app looked like it started and did nothing
// — permanently, until the process was killed by hand.
//
// Recreate the window from the same declaration the app starts with instead,
// and give the Dock icon the same path.

use tauri::utils::config::WindowConfig;
use tauri::{AppHandle, Manager, WebviewWindowBuilder};

/// Label of the window declared in `tauri.conf.json`.
pub const MAIN: &str = "main";

/// The `main` window as declared in the config — what a rebuild reproduces.
pub fn main_window_config(config: &tauri::Config) -> Option<&WindowConfig> {
    config.app.windows.iter().find(|w| w.label == MAIN)
}

/// Show the main window, recreating it when it is gone.
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        // Focus alone does not raise a minimized or hidden window.
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let Some(config) = main_window_config(app.config()).cloned() else {
        return;
    };
    match WebviewWindowBuilder::from_config(app, &config).and_then(|b| b.build()) {
        Ok(w) => {
            let _ = w.set_focus();
        }
        // Nothing else can put a window on screen, so say why on the way out.
        Err(e) => eprintln!("kumiho-desktop: could not recreate the {MAIN} window: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{main_window_config, MAIN};

    // Recovery rebuilds the window by label. Rename it in tauri.conf.json and
    // there is nothing left to rebuild from, which is the state that made the
    // app unopenable in the first place.
    #[test]
    fn the_shipped_config_declares_the_window_recovery_rebuilds() {
        let config: tauri::Config = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("tauri.conf.json parses as an app config");
        let main = main_window_config(&config).expect("tauri.conf.json declares a `main` window");
        assert_eq!(main.label, MAIN);
    }

    #[test]
    fn a_config_without_that_window_offers_nothing_to_rebuild() {
        let config: tauri::Config = serde_json::from_str(
            r#"{"identifier":"io.kumiho.desktop","app":{"windows":[{"label":"settings"}]}}"#,
        )
        .expect("minimal app config parses");
        assert!(main_window_config(&config).is_none());
    }
}
