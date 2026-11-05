use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;
use time::format_description::well_known::Rfc3339;
use url::Url;

/// Information about an available update
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateInfo {
    /// The version number of the update
    pub version: String,
    /// The release date
    pub date: Option<String>,
    /// Release notes/changelog
    pub body: Option<String>,
    /// Whether an update is currently available
    pub available: bool,
}

/// Progress information during download
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    /// Number of bytes downloaded
    pub downloaded: u64,
    /// Total bytes to download (if known)
    pub total: Option<u64>,
    /// Progress percentage (0-100)
    pub percentage: u8,
}

/// Check if the updater should be enabled
/// Returns false in development mode or when running as portable
#[tauri::command]
pub fn is_updater_available(_app: AppHandle) -> bool {
    // Disable updater in development mode
    if tauri::is_dev() {
        tracing::info!("Updater disabled: running in development mode");
        return false;
    }

    // Check if running in portable mode (not installed in Program Files or AppData)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let exe_dir_str = exe_dir.to_string_lossy().to_lowercase();

            // Installed apps are typically in Program Files or AppData
            let is_installed = exe_dir_str.contains("program files")
                || exe_dir_str.contains("programfiles")
                || exe_dir_str.contains(r"appdata\local\programs");

            // Also check for common uninstaller names which indicate an installation
            let has_uninstaller =
                exe_dir.join("unins000.exe").exists() || exe_dir.join("uninstall.exe").exists();

            if !is_installed && !has_uninstaller {
                tracing::info!(
                    "Updater disabled: running in portable mode from {:?}",
                    exe_dir
                );
                return false;
            }
        }
    }

    tracing::info!("Updater available");
    true
}

/// Check for available updates
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<UpdateInfo, String> {
    tracing::info!("Checking for updates...");

    // Check if updater is available
    if !is_updater_available(app.clone()) {
        return Ok(UpdateInfo {
            version: String::new(),
            date: None,
            body: None,
            available: false,
        });
    }

    // Read update channel from settings
    use tauri_plugin_store::StoreBuilder;
    let channel = {
        let store = StoreBuilder::new(&app, crate::profiles::settings_store_path(&app))
            .build()
            .map_err(|e| format!("Failed to load settings: {}", e))?;

        store
            .get("settings")
            .and_then(|v| v.get("settings").cloned())
            .and_then(|v| v.get("update_channel").cloned())
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "stable".to_string())
    };

    // Construct endpoint URL based on channel
    let endpoint_url = match channel.as_str() {
        "beta" => "https://github.com/Nolyo/lexena/releases/latest/download/latest-beta.json",
        _ => "https://github.com/Nolyo/lexena/releases/latest/download/latest.json", // Default to stable
    };

    tracing::info!(
        "Checking for updates on channel '{}' from: {}",
        channel,
        endpoint_url
    );

    // Parse URL
    let url =
        Url::parse(endpoint_url).map_err(|e| format!("Failed to parse endpoint URL: {}", e))?;

    // Use updater_builder to set dynamic endpoint
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| format!("Failed to set endpoints: {}", e))?
        .build()
        .map_err(|e| format!("Failed to build updater: {}", e))?;

    let update = updater
        .check()
        .await
        .map_err(|e| format!("Failed to check for updates: {}", e))?;

    match update {
        Some(update) => {
            tracing::info!("Update available: {}", update.version);

            // Convert date to RFC 3339 string (ISO 8601) so JS Date can parse it
            let date_str = update.date.and_then(|d| d.format(&Rfc3339).ok());

            Ok(UpdateInfo {
                version: update.version.clone(),
                date: date_str,
                body: update.body.clone(),
                available: true,
            })
        }
        None => {
            tracing::info!("No updates available");

            Ok(UpdateInfo {
                version: String::new(),
                date: None,
                body: None,
                available: false,
            })
        }
    }
}

/// Download and install an update
#[tauri::command]
pub async fn download_and_install_update(app: AppHandle) -> Result<(), String> {
    tracing::info!("Starting update download...");

    // Read update channel from settings
    use tauri_plugin_store::StoreBuilder;
    let channel = {
        let store = StoreBuilder::new(&app, crate::profiles::settings_store_path(&app))
            .build()
            .map_err(|e| format!("Failed to load settings: {}", e))?;

        store
            .get("settings")
            .and_then(|v| v.get("settings").cloned())
            .and_then(|v| v.get("update_channel").cloned())
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "stable".to_string())
    };

    // Construct endpoint URL based on channel
    let endpoint_url = match channel.as_str() {
        "beta" => "https://github.com/Nolyo/lexena/releases/latest/download/latest-beta.json",
        _ => "https://github.com/Nolyo/lexena/releases/latest/download/latest.json",
    };

    tracing::info!(
        "Downloading update from channel '{}': {}",
        channel,
        endpoint_url
    );

    // Parse URL
    let url =
        Url::parse(endpoint_url).map_err(|e| format!("Failed to parse endpoint URL: {}", e))?;

    // Use updater_builder to set dynamic endpoint
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| format!("Failed to set endpoints: {}", e))?
        .build()
        .map_err(|e| format!("Failed to build updater: {}", e))?;

    let update = updater
        .check()
        .await
        .map_err(|e| format!("Failed to check for updates: {}", e))?;

    if let Some(update) = update {
        tracing::info!("Downloading update version {}...", update.version);

        // Download with progress tracking
        let bytes_downloaded = std::sync::Arc::new(parking_lot::Mutex::new(0u64));
        let bytes_total = std::sync::Arc::new(parking_lot::Mutex::new(None));

        let bytes = update
            .download(
                |chunk_length, content_length| {
                    // Update progress
                    let mut downloaded = bytes_downloaded.lock();
                    *downloaded += chunk_length as u64;

                    if let Some(total) = content_length {
                        let mut total_stored = bytes_total.lock();
                        *total_stored = Some(total);
                    }

                    // Calculate percentage
                    let current_downloaded = *downloaded;
                    let current_total = *bytes_total.lock();

                    let percentage = if let Some(total) = current_total {
                        ((current_downloaded as f64 / total as f64) * 100.0).min(100.0) as u8
                    } else {
                        0
                    };

                    // Emit progress event to frontend
                    let _ = app.emit(
                        "update-download-progress",
                        DownloadProgress {
                            downloaded: current_downloaded,
                            total: current_total,
                            percentage,
                        },
                    );

                    tracing::debug!(
                        "Download progress: {} / {:?} ({}%)",
                        current_downloaded,
                        current_total,
                        percentage
                    );
                },
                || {
                    tracing::info!("Download completed successfully");
                },
            )
            .await
            .map_err(|e| format!("Failed to download and verify update: {}", e))?;

        // Download keeps normal use available and verifies the existing signed
        // installer before maintenance. Then drain durable commits and close
        // stdio bridges so the installer can replace both shipped executables.
        #[cfg(windows)]
        if let Err(error) = crate::mcp::lifecycle::prepare_update(&app).await {
            crate::mcp::lifecycle::resume(&app).await;
            return Err(error);
        }
        if let Err(error) = update.install(bytes) {
            #[cfg(windows)]
            crate::mcp::lifecycle::resume(&app).await;
            return Err(format!("Failed to install update: {}", error));
        }

        tracing::info!("Restarting application...");

        // Restart the application
        app.restart();
    } else {
        Err("No update available".to_string())
    }
}
