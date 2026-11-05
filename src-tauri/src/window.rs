use std::sync::Arc;

use serde_json::{Value, json};
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Position, Runtime, Size,
    WebviewWindow, WindowEvent,
};
use tauri_plugin_store::StoreBuilder;

pub(crate) const DEFAULT_MINI_WIDTH: f64 = 246.0;
pub(crate) const DEFAULT_MINI_HEIGHT: f64 = 57.0;

pub(crate) fn parse_geometry(value: &str) -> Option<(u32, u32, i32, i32)> {
    let mut parts = value.split('+');
    let size_part = parts.next()?;
    let mut size_split = size_part.split('x');
    let width = size_split.next()?.parse::<u32>().ok()?;
    let height = size_split.next()?.parse::<u32>().ok()?;
    let x = parts.next()?.parse::<i32>().ok()?;
    let y = parts.next()?.parse::<i32>().ok()?;
    Some((width, height, x, y))
}

pub(crate) fn format_geometry(size: PhysicalSize<u32>, position: PhysicalPosition<i32>) -> String {
    format!(
        "{}x{}+{}+{}",
        size.width, size.height, position.x, position.y
    )
}

pub(crate) fn update_window_settings<R: Runtime>(
    store: &Arc<tauri_plugin_store::Store<R>>,
    geometry: Option<String>,
    state: Option<String>,
) {
    let mut data = store.get("settings").unwrap_or_else(|| json!({}));
    if !data.is_object() {
        data = json!({});
    }

    {
        let root = data
            .as_object_mut()
            .expect("settings root should be an object");
        let settings_value = root.entry("settings").or_insert_with(|| json!({}));

        if !settings_value.is_object() {
            *settings_value = json!({});
        }

        if let Some(settings_obj) = settings_value.as_object_mut() {
            if let Some(geom) = geometry {
                settings_obj.insert("main_window_geometry".into(), json!(geom));
            }
            if let Some(state_str) = state {
                settings_obj.insert("main_window_state".into(), json!(state_str));
            }
        }
    }

    store.set("settings", data);
}

fn update_mini_window_geometry<R: Runtime>(
    store: &Arc<tauri_plugin_store::Store<R>>,
    geometry: String,
) {
    let mut data = store.get("settings").unwrap_or_else(|| json!({}));
    if !data.is_object() {
        data = json!({});
    }

    {
        let root = data
            .as_object_mut()
            .expect("settings root should be an object");
        let settings_value = root.entry("settings").or_insert_with(|| json!({}));

        if !settings_value.is_object() {
            *settings_value = json!({});
        }

        if let Some(settings_obj) = settings_value.as_object_mut() {
            settings_obj.insert("mini_window_geometry".into(), json!(geometry));
        }
    }

    store.set("settings", data);
}

pub(crate) fn capture_mini_window_state<R: Runtime>(
    window: &WebviewWindow<R>,
    store: &Arc<tauri_plugin_store::Store<R>>,
) {
    // Tauri's set_size() sets the *inner* (client) size, so we must store the
    // inner size too — otherwise each round-trip (save outer → restore as inner)
    // drifts by the DWM extended frame width on Windows.
    if let (Ok(size), Ok(position)) = (window.inner_size(), window.outer_position()) {
        update_mini_window_geometry(store, format_geometry(size, position));
    }
}

fn geometry_inside_any_work_area<R: Runtime>(
    app_handle: &AppHandle<R>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> bool {
    let monitors = match app_handle.available_monitors() {
        Ok(list) => list,
        Err(_) => return false,
    };

    for monitor in monitors {
        let area = monitor.work_area();
        let left = area.position.x;
        let top = area.position.y;
        let right = left + area.size.width as i32;
        let bottom = top + area.size.height as i32;

        let win_right = x + width as i32;
        let win_bottom = y + height as i32;

        let horizontally_inside = x < right && win_right > left;
        let vertically_inside = y < bottom && win_bottom > top;

        if horizontally_inside && vertically_inside {
            return true;
        }
    }

    false
}

pub(crate) fn restore_mini_geometry<R: Runtime>(
    app_handle: &AppHandle<R>,
    window: &WebviewWindow<R>,
    store: &Arc<tauri_plugin_store::Store<R>>,
) -> bool {
    let Some(settings_value) = store.get("settings") else {
        return false;
    };

    let Some(settings_obj) = settings_value.get("settings").and_then(Value::as_object) else {
        return false;
    };

    let Some(geometry_str) = settings_obj
        .get("mini_window_geometry")
        .and_then(Value::as_str)
    else {
        return false;
    };

    if geometry_str.is_empty() {
        return false;
    }

    let Some((width, height, x, y)) = parse_geometry(geometry_str) else {
        return false;
    };

    if !geometry_inside_any_work_area(app_handle, x, y, width, height) {
        return false;
    }

    if let Err(err) = window.set_size(Size::Physical(PhysicalSize { width, height })) {
        eprintln!("[mini-window] Failed to apply size: {}", err);
        return false;
    }
    if let Err(err) = window.set_position(Position::Physical(PhysicalPosition { x, y })) {
        eprintln!("[mini-window] Failed to apply position: {}", err);
        return false;
    }

    true
}

pub(crate) fn capture_window_state<R: Runtime>(
    window: &WebviewWindow<R>,
    store: &Arc<tauri_plugin_store::Store<R>>,
) {
    let is_minimized = window.is_minimized().unwrap_or(false);
    let is_maximized = window.is_maximized().unwrap_or(false);

    let state = if is_minimized {
        "minimized".to_string()
    } else if is_maximized {
        "maximized".to_string()
    } else {
        "normal".to_string()
    };

    let geometry = if state == "normal" {
        match (window.outer_size(), window.outer_position()) {
            (Ok(size), Ok(position)) => Some(format_geometry(size, position)),
            _ => None,
        }
    } else {
        None
    };

    update_window_settings(store, geometry, Some(state));
}

pub(crate) fn restore_window_state<R: Runtime>(
    window: &WebviewWindow<R>,
    store: &Arc<tauri_plugin_store::Store<R>>,
) {
    let Some(settings_value) = store.get("settings") else {
        return;
    };

    let Some(settings_obj) = settings_value.get("settings").and_then(Value::as_object) else {
        return;
    };

    if let Some(geometry_str) = settings_obj
        .get("main_window_geometry")
        .and_then(Value::as_str)
    {
        if let Some((width, height, x, y)) = parse_geometry(geometry_str) {
            if let Err(err) = window.set_size(Size::Physical(PhysicalSize { width, height })) {
                eprintln!("[window-state] Failed to apply window size: {}", err);
            }
            if let Err(err) = window.set_position(Position::Physical(PhysicalPosition { x, y })) {
                eprintln!("[window-state] Failed to apply window position: {}", err);
            }
        }
    }

    if let Some(state_str) = settings_obj
        .get("main_window_state")
        .and_then(Value::as_str)
    {
        match state_str {
            "maximized" => {
                if let Err(err) = window.maximize() {
                    eprintln!("[window-state] Failed to maximize window: {}", err);
                }
            }
            "minimized" => {
                if let Err(err) = window.minimize() {
                    eprintln!("[window-state] Failed to minimize window: {}", err);
                }
            }
            _ => {
                let _ = window.unmaximize();
                let _ = window.unminimize();
            }
        }
    }
}

pub(crate) fn position_mini_window<R: Runtime>(app_handle: &AppHandle<R>, window: &WebviewWindow<R>) {
    const MARGIN_BOTTOM: i32 = 32;

    let window_size = window
        .outer_size()
        .ok()
        .unwrap_or_else(|| {
            PhysicalSize::new(DEFAULT_MINI_WIDTH as u32, DEFAULT_MINI_HEIGHT as u32)
        });

    let target_monitor = app_handle
        .get_webview_window("main")
        .and_then(|main| main.current_monitor().ok().flatten())
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());

    let Some(monitor) = target_monitor else {
        return;
    };

    // Use work_area (excludes Windows taskbar, macOS Dock/menubar, Linux panels)
    // instead of full monitor size so the mini never ends up under the taskbar.
    let work_area = monitor.work_area();
    let area_size = work_area.size;
    let area_position = work_area.position;

    let window_width = window_size.width as i32;
    let window_height = window_size.height as i32;

    let centered_x = area_position.x + ((area_size.width as i32 - window_width) / 2);
    let bottom_y = area_position.y + area_size.height as i32 - window_height - MARGIN_BOTTOM;

    let new_position = PhysicalPosition {
        x: centered_x,
        y: bottom_y.max(area_position.y),
    };

    let _ = window.set_position(Position::Physical(new_position));
}

pub(crate) fn show_mini_window<R: Runtime>(app_handle: &AppHandle<R>) {
    if let Some(mini_window) = app_handle.get_webview_window("mini") {
        position_mini_window(app_handle, &mini_window);

        #[cfg(windows)]
        {
            if let Ok(hwnd) = mini_window.hwnd() {
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
                    ShowWindow,
                };
                const SW_SHOWNOACTIVATE: i32 = 4;
                unsafe {
                    // Re-assert HWND_TOPMOST on every show: the always_on_top flag
                    // set at creation can be lost (another topmost window focuses,
                    // session switch, explorer restart), leaving the mini behind
                    // the taskbar.
                    SetWindowPos(
                        hwnd.0,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    ShowWindow(hwnd.0, SW_SHOWNOACTIVATE);
                }
                return;
            }
        }

        let _ = mini_window.show();
    }
}

pub(crate) fn hide_mini_window<R: Runtime>(app_handle: &AppHandle<R>) {
    if let Some(mini_window) = app_handle.get_webview_window("mini") {
        #[cfg(windows)]
        {
            if let Ok(hwnd) = mini_window.hwnd() {
                const SW_HIDE: i32 = 0;
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                        hwnd.0,
                        SW_HIDE,
                    );
                }
                return;
            }
        }
        let _ = mini_window.hide();
    }
}

/// Create the mini visualizer window at startup (hidden by default)
pub(crate) fn create_mini_window(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::WebviewUrl;
    use tauri::WebviewWindowBuilder;

    let mini = WebviewWindowBuilder::new(app, "mini", WebviewUrl::App("mini.html".into()))
        .title("Lexena - Mini")
        .inner_size(DEFAULT_MINI_WIDTH, DEFAULT_MINI_HEIGHT)
        .min_inner_size(180.0, 36.0)
        .resizable(true)
        .decorations(false)
        .always_on_top(true)
        .transparent(true)
        .visible(false)
        .focusable(false)
        .build()?;

    // Share the same underlying store instance as setup_main_window
    // (StoreBuilder returns the cached Arc<Store> when the path already exists).
    let settings_path = crate::profiles::settings_store_path(app);
    let mini_store = StoreBuilder::new(app, settings_path).build()?;

    if !restore_mini_geometry(app, &mini, &mini_store) {
        position_mini_window(app, &mini);
    }

    let events_window = mini.clone();
    let events_store = mini_store.clone();
    mini.on_window_event(move |event| match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            capture_mini_window_state(&events_window, &events_store);
        }
        _ => {}
    });

    Ok(())
}

pub(crate) const VOICE_EDIT_WIDTH: f64 = 560.0;
pub(crate) const VOICE_EDIT_HEIGHT: f64 = 320.0;

/// Create the Voice Edit overlay at startup (hidden by default).
///
/// Pre-created like the mini window: building a webview on demand costs enough
/// to be visible, and this overlay opens on a keystroke.
pub(crate) fn create_voice_edit_window(
    app: &tauri::AppHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::WebviewUrl;
    use tauri::WebviewWindowBuilder;

    WebviewWindowBuilder::new(
        app,
        "voice-edit",
        WebviewUrl::App("voice-edit.html".into()),
    )
    .title("Lexena - Voice Edit")
    .inner_size(VOICE_EDIT_WIDTH, VOICE_EDIT_HEIGHT)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .transparent(true)
    .visible(false)
    .skip_taskbar(true)
    // Unlike the mini window, this one must take focus: the palette is driven
    // by digit keys and Escape, captured by the webview itself rather than by
    // extra global shortcuts.
    .focusable(true)
    .center()
    .build()?;

    Ok(())
}

pub(crate) fn show_voice_edit_window<R: Runtime>(app_handle: &AppHandle<R>) {
    if let Some(window) = app_handle.get_webview_window("voice-edit") {
        let _ = window.center();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub(crate) fn hide_voice_edit_window<R: Runtime>(app_handle: &AppHandle<R>) {
    if let Some(window) = app_handle.get_webview_window("voice-edit") {
        let _ = window.hide();
    }
}

/// Renderer-facing hide, called when the overlay dismisses itself (Escape, or
/// after a successful replace).
#[tauri::command]
pub fn hide_voice_edit_overlay(app_handle: AppHandle) {
    hide_voice_edit_window(&app_handle);
}

pub(crate) const DEFAULT_NOTE_WIDTH: f64 = 520.0;
pub(crate) const DEFAULT_NOTE_HEIGHT: f64 = 640.0;
const NOTE_CASCADE_OFFSET_PX: i32 = 32;

pub(crate) fn note_window_label(note_id: &str) -> String {
    format!("note-{note_id}")
}

/// Position a freshly-created (still hidden) note window: at the OS cursor
/// (drag-out drop) or centered on the main window with a cascade offset so
/// several detached notes don't stack exactly on top of each other.
/// All coordinates are physical pixels — no DPI math in JS.
fn position_note_window(
    app: &tauri::AppHandle,
    window: &WebviewWindow,
    at_cursor: bool,
    existing_note_windows: usize,
) {
    if at_cursor {
        if let Ok(cursor) = app.cursor_position() {
            // Negative physical coordinates are valid on multi-monitor setups
            // (a monitor left of/above the primary) — must not be clamped.
            let _ = window.set_position(Position::Physical(PhysicalPosition {
                x: cursor.x as i32 - 60,
                y: cursor.y as i32 - 20,
            }));
            return;
        }
    }
    let size = window
        .outer_size()
        .ok()
        .unwrap_or_else(|| PhysicalSize::new(DEFAULT_NOTE_WIDTH as u32, DEFAULT_NOTE_HEIGHT as u32));
    let Some(main) = app.get_webview_window("main") else {
        return;
    };
    let (Ok(pos), Ok(main_size)) = (main.outer_position(), main.outer_size()) else {
        return;
    };
    let offset = NOTE_CASCADE_OFFSET_PX * existing_note_windows as i32;
    let x = pos.x + (main_size.width as i32 - size.width as i32) / 2 + offset;
    let y = pos.y + (main_size.height as i32 - size.height as i32) / 2 + offset;
    let _ = window.set_position(Position::Physical(PhysicalPosition { x, y }));
}

/// Create the detached window for a note, or focus it if it already exists.
pub(crate) fn open_note_window(
    app: &tauri::AppHandle,
    note_id: &str,
    at_cursor: bool,
) -> Result<(), String> {
    use tauri::{Emitter, WebviewUrl, WebviewWindowBuilder};

    let label = note_window_label(note_id);

    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.show();
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(());
    }

    let existing_note_windows = app
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("note-"))
        .count();

    let window = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("note.html?noteId={note_id}").into()),
    )
    .title("Lexena")
    .inner_size(DEFAULT_NOTE_WIDTH, DEFAULT_NOTE_HEIGHT)
    .min_inner_size(320.0, 240.0)
    .resizable(true)
    .visible(false)
    .build()
    .map_err(|e| format!("Failed to create note window: {e}"))?;

    position_note_window(app, &window, at_cursor, existing_note_windows);

    // Closing the window IS the reattach signal (spec §4): the main window
    // restores the tab silently. Flows that must NOT restore it (delete,
    // explicit reattach) remove the id from `detachedNoteIds` BEFORE closing,
    // and the main-window handler ignores ids absent from the registry.
    let app_handle = app.clone();
    let closed_note_id = note_id.to_string();
    let opened_session = crate::note_service::editors::editors().window_session(&label);
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            crate::note_service::editors::editors().window_destroyed(&note_window_label(&closed_note_id));
            let _ = app_handle.emit(
                "note-window-closed",
                serde_json::json!({ "noteId": closed_note_id, "profile_id":opened_session.as_ref().map(|s|&s.profile_id), "session_generation":opened_session.as_ref().map(|s|s.session_generation) }),
            );
        }
    });

    let _ = window.show();
    let _ = window.set_focus();

    Ok(())
}

/// Setup main window: restore state, register event handlers, handle --minimized flag
pub(crate) fn setup_main_window(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let has_minimized_flag = args
        .iter()
        .any(|arg| arg == "--minimized" || arg == "--hidden");

    let settings_path = crate::profiles::settings_store_path(&app.handle());
    let window_store = StoreBuilder::new(app, settings_path).build()?;

    let should_start_minimized = if has_minimized_flag {
        let start_minimized = window_store
            .get("settings")
            .and_then(|settings_root| {
                settings_root.get("settings").and_then(|settings_obj| {
                    settings_obj
                        .get("start_minimized_on_boot")
                        .and_then(|v| v.as_bool())
                })
            })
            .unwrap_or(true);
        start_minimized
    } else {
        false
    };

    if let Some(window) = app.get_webview_window("main") {
        restore_window_state(&window, &window_store);

        if !should_start_minimized {
            let _ = window.show();
        }

        let events_store = window_store.clone();
        let events_window = window.clone();
        window.on_window_event(move |event| {
            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    capture_window_state(&events_window, &events_store);
                    if let Err(err) = events_store.save() {
                        eprintln!("[window-state] Failed to save settings: {}", err);
                    }
                    api.prevent_close();
                    let _ = events_window.hide();
                }
                WindowEvent::Resized(_)
                | WindowEvent::Moved(_)
                | WindowEvent::ScaleFactorChanged { .. } => {
                    capture_window_state(&events_window, &events_store);
                }
                _ => {}
            }
        });
    }

    Ok(())
}

#[cfg(test)]
mod note_window_tests {
    use super::note_window_label;

    #[test]
    fn label_prefixes_note_id() {
        assert_eq!(
            note_window_label("0f8fad5b-d9cb-469f-a165-70867728950e"),
            "note-0f8fad5b-d9cb-469f-a165-70867728950e"
        );
    }
}
