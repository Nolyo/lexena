mod audio;
mod audio_trim;
mod auth;
mod billing;
mod cloud;
mod commands;
mod folders;
mod hotkeys;
mod logging;
mod logs;
#[cfg(windows)]
mod mcp;
mod note_service;
mod notes;
mod profiles;
mod state;
mod streaming;
mod sync;
mod sync_queue;
mod transcription;
mod transcription_local;
mod transcriptions;
mod tray;
mod updater;
mod voice_edit;
mod window;

// Re-export for transcription_local compatibility
pub use state::{AppState, WhisperCache, WhisperState};

use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Route a `lexena://...` URL to the appropriate subsystem.
///
/// Today we recognize two destinations:
/// - `lexena://auth/callback?...` — Supabase Auth (magiclink, oauth, signup, recovery, email_change).
///   Forwarded to `auth::emit_deep_link_event` which validates the URL shape, buffers
///   the payload for cold-start, and emits `auth-deep-link-received`.
/// - `lexena://billing/success` — Lemon Squeezy checkout success redirect.
///   Emits `billing-checkout-completed` (no payload) so the `CloudContext` can
///   re-fetch subscription state, and brings the main window forward.
///
/// Unknown paths fall through to the auth handler (which will reject them as
/// `wrong host/path`); this keeps the routing cheap and the rejection logged.
fn route_deep_link<R: Runtime>(app: &AppHandle<R>, url: &str) {
    if let Ok(parsed) = url::Url::parse(url) {
        if parsed.scheme() == "lexena"
            && parsed.host_str() == Some("billing")
            && parsed.path() == "/success"
        {
            tracing::info!(target: "billing", "billing/success deep link received");
            if let Err(e) = app.emit("billing-checkout-completed", ()) {
                tracing::warn!("failed to emit billing-checkout-completed: {}", e);
            }
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.set_focus();
            }
            return;
        }
    }
    auth::emit_deep_link_event(app, url);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize logging system
    let log_layer = logging::init_logging();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let has_deep_link = args.iter().any(|a| a.starts_with("lexena://"));
            tracing::info!(
                "single_instance fired (arg_count={}, has_deep_link={})",
                args.len(),
                has_deep_link
            );
            // ─── Route deep-link args (auth or billing) ───────────────────────────────
            if let Some(url) = args.iter().find(|a| a.starts_with("lexena://")) {
                route_deep_link(app, url);
            }
            // ─── Preserve existing behavior (bring window forward) ────────────────────
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state::create_app_state())
        .invoke_handler(tauri::generate_handler![
            commands::files::greet,
            commands::recording::get_audio_devices,
            commands::recording::start_recording,
            commands::recording::stop_recording,
            commands::recording::is_recording,
            commands::recording::start_audio_monitor,
            commands::recording::stop_audio_monitor,
            commands::window::exit_app,
            commands::window::close_mini_window,
            commands::window::save_mini_window_geometry,
            commands::window::recenter_mini_window,
            commands::window::open_note_window,
            commands::window::close_note_window,
            commands::window::show_main_window,
            commands::files::log_separator,
            commands::files::open_app_data_dir,
            commands::settings::is_autostart_enabled,
            commands::settings::set_autostart,
            commands::settings::update_hotkeys,
            commands::transcription::transcribe_audio,
            commands::transcription::load_recording,
            commands::misc::paste_text_to_active_window,
            commands::misc::type_text_at_cursor,
            commands::selection::replace_selection,
            voice_edit::stop_voice_edit_instruction,
            window::hide_voice_edit_overlay,
            commands::misc::frontend_log,
            updater::check_for_updates,
            updater::download_and_install_update,
            updater::is_updater_available,
            commands::settings::get_update_channel,
            commands::settings::set_update_channel,
            commands::settings::set_translate_mode,
            commands::settings::set_post_process_enabled,
            commands::settings::set_cloud_gate,
            commands::settings::set_streaming_enabled,
            commands::model::download_local_model,
            commands::model::check_local_model_exists,
            commands::model::any_local_model_exists,
            commands::model::delete_local_model,
            commands::system::get_system_info,
            commands::system::get_device_info,
            commands::files::delete_recording_files,
            notes::list_notes,
            commands::note_editors::get_profile_session,
            commands::note_editors::register_note_editor,
            commands::note_editors::read_editor_note,
            commands::note_editors::read_note_metadata_batch,
            commands::note_editors::begin_note_edit,
            commands::note_editors::save_editor_note,
            commands::note_editors::ack_editor_prepare,
            commands::note_editors::end_note_edit,
            commands::sync_queue::load_sync_queue,
            commands::sync_queue::save_sync_queue,
            #[cfg(windows)]
            commands::mcp::mcp_get_status,
            #[cfg(windows)]
            commands::mcp::mcp_set_enabled,
            #[cfg(windows)]
            commands::mcp::mcp_create_connection,
            #[cfg(windows)]
            commands::mcp::mcp_revoke_connection,
            #[cfg(windows)]
            commands::mcp::mcp_rotate_connection,
            #[cfg(windows)]
            commands::mcp::mcp_get_client_config,
            #[cfg(windows)]
            commands::mcp::mcp_get_audit,
            #[cfg(windows)]
            commands::mcp::mcp_test_connection,
            #[cfg(windows)]
            commands::mcp::mcp_reset_installation,
            commands::local_mutations::list_local_mutations,
            commands::local_mutations::ack_local_mutations,
            commands::local_mutations::apply_sync_remote_note,
            commands::local_mutations::apply_sync_remote_folder,
            commands::local_mutations::purge_sync_tombstones,
            commands::local_mutations::seed_local_sync_snapshot,
            notes::read_note,
            notes::create_note,
            notes::update_note,
            notes::delete_note,
            notes::search_notes,
            notes::get_backlinks,
            notes::toggle_note_favorite,
            notes::set_note_local_only,
            notes::move_note_to_folder,
            notes::reorder_notes_in_folder,
            notes::purge_soft_deleted_notes_post_pull,
            notes::import_note_for_backup,
            folders::list_folders,
            folders::create_folder,
            folders::rename_folder,
            folders::delete_folder,
            folders::reorder_folders,
            folders::purge_soft_deleted_folders_post_pull,
            folders::import_folders_for_backup,
            transcriptions::list_transcriptions,
            transcriptions::save_transcription,
            transcriptions::delete_transcription,
            transcriptions::clear_transcriptions,
            transcriptions::update_transcription,
            transcriptions::export_transcriptions,
            logs::list_logs,
            logs::save_log,
            logs::clear_logs,
            tray::update_tray_labels,
            commands::profiles::list_profiles,
            commands::profiles::get_active_profile,
            commands::profiles::get_active_profile_settings_path,
            commands::profiles::get_active_profile_notes_tabs_path,
            commands::profiles::get_active_profile_notes_sidebar_path,
            commands::profiles::get_active_profile_sync_meta_path,
            commands::profiles::get_profile_sync_meta_path,
            commands::profiles::get_active_profile_sync_queue_path,
            commands::profiles::get_active_profile_snippets_path,
            commands::profiles::get_active_profile_dictionary_path,
            commands::profiles::create_profile,
            commands::profiles::rename_profile,
            commands::profiles::delete_profile,
            commands::profiles::switch_profile,
            commands::profiles::set_profile_avatar,
            commands::profiles::get_profile_avatar,
            commands::profiles::clear_profile_avatar,
            commands::reset::reset_app_data,
            auth::store_refresh_token,
            auth::get_refresh_token,
            auth::clear_refresh_token,
            auth::get_or_create_device_id,
            auth::generate_oauth_state,
            auth::consume_pending_deep_link,
            sync::write_local_backup,
            sync::list_local_backups,
            sync::read_local_backup,
            sync::delete_local_backup,
            sync::delete_all_local_backups,
            sync::save_export_to_download,
            cloud::transcribe_audio_cloud,
            cloud::post_process_cloud,
            cloud::notes_assist_cloud,
            billing::open_checkout,
        ])
        .setup(move |app| {
            // ─── Deep-link: subscribe to live on_open_url events ──────────────────────
            use tauri_plugin_deep_link::DeepLinkExt;
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                let urls: Vec<_> = event.urls().into_iter().collect();
                tracing::info!("on_open_url fired (url_count={})", urls.len());
                for url in urls {
                    let s = url.as_str();
                    if s.starts_with("lexena://") {
                        route_deep_link(&handle, s);
                    }
                }
            });

            // Enable logging to frontend
            log_layer.set_app_handle(app.handle().clone());

            tracing::info!("Lexena v{} started", app.package_info().version);

            // Profile system: migrate legacy data then init active profile
            if let Err(e) = profiles::migrate_legacy_to_default(app.handle()) {
                tracing::warn!("Profiles migration failed: {}", e);
            }
            if let Err(e) = profiles::init_active_profile(app.handle()) {
                tracing::error!("Failed to initialize active profile: {}", e);
            }
            if let Err(e) = profiles::cleanup_legacy_root_sync_stores(app.handle()) {
                tracing::warn!("Legacy root sync store cleanup failed: {}", e);
            }
            if let Err(e) = profiles::migrate_global_snippets_dict_to_default(app.handle()) {
                tracing::warn!("Global snippets/dict migration failed: {}", e);
            }

            // Migrations
            if let Err(e) = transcriptions::cleanup_legacy_transcriptions(app.handle()) {
                tracing::warn!("Failed to cleanup legacy transcriptions: {}", e);
            }
            if let Err(e) = logs::cleanup_legacy_logs(app.handle()) {
                tracing::warn!("Failed to cleanup legacy logs: {}", e);
            }
            match notes::migrate_notes_from_store(app.handle()) {
                Ok(count) if count > 0 => {
                    tracing::info!("Notes migration completed: {} notes migrated", count);
                }
                Err(e) => {
                    tracing::warn!("Notes migration failed: {}", e);
                }
                _ => {}
            }

            // Replay durable intents before any renderer, sync, or MCP session
            // can read a partially applied transaction. A failed profile stays closed.
            for profile in profiles::load_manifest(app.handle())?.profiles {
                if let Err(error) = note_service::ProfileScope::resolve(app.handle(), &profile.id)
                    .and_then(|scope| {
                        note_service::journal::Journal::recover_profile(&scope)?;
                        note_service::journal::Journal::compact(&scope)
                    })
                {
                    // Every repository call repeats this gate. An unrecoverable
                    // profile remains closed while other app functions work.
                    tracing::error!(
                        "Note recovery refused for profile {}: {}",
                        profile.id,
                        error
                    );
                }
            }
            #[cfg(windows)]
            {
                let data = app.path().app_data_dir()?;
                let executable = std::env::current_exe()?;
                let local = commands::mcp::LocalMcpState::new(data.clone(), &executable)?;
                let server = mcp::server::McpServer::new(
                    data,
                    local.grants.clone(),
                    std::sync::Arc::new(mcp::native::TauriEditors::new(app.handle().clone())),
                );
                local.set_handler(server.session_handler())?;
                app.manage(server);
                app.manage(local);
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = handle
                        .state::<commands::mcp::LocalMcpState>()
                        .start_if_enabled()
                        .await
                    {
                        tracing::warn!("Local MCP startup failed: {}", error);
                    }
                });
            }

            let maintenance_app_data = app.path().app_data_dir()?;
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(6 * 60 * 60)).await;
                    let data = maintenance_app_data.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        if let Ok(manifest) = note_service::ProfileRegistry::manifest_in(&data) {
                            for profile in manifest.profiles {
                                if let Err(error) =
                                    note_service::ProfileScope::resolve_in(&data, &profile.id)
                                        .and_then(|scope| {
                                            note_service::journal::Journal::compact(&scope)
                                        })
                                {
                                    tracing::warn!(
                                        "Note history maintenance refused for profile {}: {}",
                                        profile.id,
                                        error
                                    );
                                }
                            }
                        }
                    })
                    .await;
                }
            });

            // Window setup
            window::create_mini_window(&app.handle())?;
            window::create_voice_edit_window(&app.handle())?;
            window::setup_main_window(app)?;

            // Hotkeys
            hotkeys::setup_initial_hotkeys(app)?;

            // Whisper preload in background (if Local provider is configured)
            transcription_local::preload_if_configured(app);

            // System tray
            tray::setup_tray(app)?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
