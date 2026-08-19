// Bringing Desktop back to the front.
//
// `single_instance` makes every launch after the first exit cleanly and hand
// the request to the process already running, so what that process does here
// is the app's only way back onto the screen. It used to call `set_focus()`
// and give up when the window was missing, which left two states with no way
// out: once in them, every later launch looked like it did nothing at all.
//
// 1. The window is still there but focus alone cannot raise it — minimized,
//    hidden, or ordered out. This is the state the report was in: destroying
//    the last window fires ExitRequested, which pkills kumiho-brain, and that
//    Brain was still running after twelve days, so the window still existed.
//    unminimize() → show() → set_focus() is what recovers it, and dropping any
//    of the three brings the bug back.
// 2. The window is really gone and the process is still up. Tauri normally
//    exits with the last window, so this needs an exit that was prevented or
//    that stalled — and then focus has nothing to act on, so only rebuilding
//    from the declaration in tauri.conf.json puts a window back.
//
// RunEvent::Reopen sends the macOS Dock icon down the same two paths.

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
        // All three, in this order — see state 1 above.
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
