//! Lock order: registry read/write guard, then profile mutex. No guards cross an await.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMeta {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilesManifest {
    pub active: String,
    pub profiles: Vec<ProfileMeta>,
}

pub fn name_to_id(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn validate_profile_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Profile name cannot be empty.".to_string());
    }
    if trimmed.len() > 64 {
        return Err("Profile name is too long (max 64 characters).".to_string());
    }
    Ok(())
}

pub fn generate_unique_id(base_id: &str, existing: &[ProfileMeta]) -> String {
    const RESERVED: &[&str] = &["models", "logs"];

    let is_taken = |id: &str| existing.iter().any(|p| p.id == id) || RESERVED.contains(&id);

    if !is_taken(base_id) {
        return base_id.to_string();
    }

    let mut counter = 2u32;
    loop {
        let candidate = format!("{}-{}", base_id, counter);
        if !is_taken(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

pub fn profile_exists(manifest: &ProfilesManifest, id: &str) -> bool {
    manifest.profiles.iter().any(|p| p.id == id)
}
use parking_lot::{Mutex, RwLock, RwLockWriteGuard};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Invalid profile or path: {0}")]
    InvalidScope(String),
    #[error("Profile no longer exists")]
    ProfileDeleted,
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("RECOVERY_REQUIRED")]
    RecoveryRequired,
    #[error("FORMAT_UNSUPPORTED")]
    FormatUnsupported,
    #[error("REVISION_CONFLICT")]
    RevisionConflict,
    #[error("OPERATION_CONFLICT")]
    OperationConflict,
    #[error("HISTORY_FULL")]
    HistoryFull,
    #[error("SNAPSHOT_EXPIRED")]
    SnapshotExpired,
    #[error("ACTIVE_NOTE_EXISTS")]
    ActiveNoteExists,
    #[error("EDITOR_BUSY")]
    EditorBusy,
    #[error("EDITOR_SESSION_EXPIRED")]
    EditorSessionExpired,
}
impl From<std::io::Error> for DomainError {
    fn from(e: std::io::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
impl From<serde_json::Error> for DomainError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
impl From<anyhow::Error> for DomainError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
impl From<String> for DomainError {
    fn from(e: String) -> Self {
        Self::Storage(e)
    }
}

static REGISTRY: RwLock<()> = RwLock::new(());
// A failed or completed reset must never admit another profile transaction in
// this process, including callbacks queued before the renderer receives a reply.
static RESET_ROOTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
static PROFILE_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
pub(crate) fn registry_write() -> RwLockWriteGuard<'static, ()> {
    REGISTRY.write()
}
fn profile_lock(root: &Path) -> Arc<Mutex<()>> {
    PROFILE_LOCKS
        .get_or_init(Default::default)
        .lock()
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}

/// Stable local identity; only a validated registry lookup can construct a scope.
#[derive(Debug, Clone)]
pub struct ProfileScope {
    local_id: String,
    root: PathBuf,
    app_data: PathBuf,
    created_at: String,
}
impl ProfileScope {
    pub fn resolve_in(app_data: &Path, profile_id: &str) -> Result<Self, DomainError> {
        let _registry = REGISTRY.read();
        Self::resolve_unlocked(app_data, profile_id)
    }
    fn resolve_unlocked(app_data: &Path, profile_id: &str) -> Result<Self, DomainError> {
        validate_id(profile_id)?;
        let app_data = fs::canonicalize(app_data)?;
        let manifest = read_manifest_unlocked(&app_data)?;
        let profile = manifest
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| DomainError::InvalidScope(profile_id.into()))?;
        let root = checked_profile_root_in(&app_data, profile_id)?;
        if !root.is_dir() {
            return Err(DomainError::InvalidScope(profile_id.into()));
        }
        Ok(Self {
            local_id: profile_id.into(),
            root: fs::canonicalize(root)?,
            app_data,
            created_at: profile.created_at.clone(),
        })
    }
    pub fn local_id(&self) -> &str {
        &self.local_id
    }
    /// Immutable registry lifetime captured during resolution; permits grant
    /// checks under an existing transaction without re-entering the registry.
    pub fn created_at(&self) -> &str {
        &self.created_at
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn validate_current(&self) -> Result<bool, DomainError> {
        let manifest = read_manifest_unlocked(&self.app_data)?;
        let profile = manifest
            .profiles
            .iter()
            .find(|p| p.id == self.local_id && p.created_at == self.created_at)
            .ok_or(DomainError::ProfileDeleted)?;
        let root = checked_profile_root_in(&self.app_data, &profile.id)?;
        if !root.is_dir() || fs::canonicalize(root)? != self.root {
            return Err(DomainError::ProfileDeleted);
        }
        Ok(manifest.active == self.local_id)
    }
    /// Common transaction entry. The closure may call locked repository methods;
    /// it must never re-enter a public repository/service method for this profile.
    pub fn transaction<T>(
        &self,
        action: impl FnOnce(&mut LockedProfile<'_>) -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        let _registry = REGISTRY.read();
        let lock = profile_lock(&self.root);
        let _profile = lock.lock();
        let active = self.validate_current()?;
        let mut profile = LockedProfile {
            scope: self,
            active,
            staged: Default::default(),
        };
        super::journal::Journal::recover_locked(&profile)?;
        let result = action(&mut profile)?;
        super::mutation::NoteService::commit_maintenance(&mut profile)?;
        Ok(result)
    }
}

/// Capability proving registry + profile locks are held. Constructor is private.
/// Downstream journal code uses this token instead of acquiring a second mutex.
pub struct LockedProfile<'a> {
    scope: &'a ProfileScope,
    // Registry read lock remains held for this token's whole lifetime, so no
    // legal profile switch can change this fact during authorization/commit.
    active: bool,
    pub(crate) staged: std::cell::RefCell<std::collections::BTreeMap<String, Option<String>>>,
}
impl LockedProfile<'_> {
    pub fn scope(&self) -> &ProfileScope {
        self.scope
    }
    pub fn is_active(&self) -> Result<bool, DomainError> {
        Ok(self.active)
    }
    fn relative(&self, path: &Path) -> Result<String, DomainError> {
        let relative = path
            .strip_prefix(self.scope.root())
            .map_err(|_| DomainError::InvalidScope("Outside profile".into()))?;
        self.path(relative)?;
        Ok(relative.to_string_lossy().replace('\\', "/"))
    }
    pub(crate) fn read_text(&self, path: impl AsRef<Path>) -> Result<String, DomainError> {
        let path = path.as_ref();
        let relative = self.relative(path)?;
        if let Some(value) = self.staged.borrow().get(&relative) {
            return value.clone().ok_or_else(|| DomainError::NotFound(relative));
        }
        Ok(fs::read_to_string(path)?)
    }
    pub(crate) fn stage_text(
        &self,
        path: impl AsRef<Path>,
        text: impl AsRef<str>,
    ) -> Result<(), DomainError> {
        let relative = self.relative(path.as_ref())?;
        self.staged
            .borrow_mut()
            .insert(relative, Some(text.as_ref().into()));
        Ok(())
    }
    pub(crate) fn stage_purge_note(&self, path: impl AsRef<Path>) -> Result<(), DomainError> {
        for name in ["note.json", "content.html"] {
            let relative = self.relative(&path.as_ref().join(name))?;
            self.staged.borrow_mut().insert(relative, None);
        }
        Ok(())
    }
    /// Reject reparse points/symlinks in every existing component before I/O.
    pub fn path(&self, relative: &Path) -> Result<PathBuf, DomainError> {
        checked_relative(&self.scope.root, relative)
    }
}

/// IDs may include Unicode (existing profile slugs), but are one safe component.
pub(crate) fn validate_id(id: &str) -> Result<(), DomainError> {
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if id.is_empty()
        || id.len() > 255
        || !id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        || reserved.contains(&id.to_ascii_uppercase().as_str())
    {
        return Err(DomainError::InvalidScope(id.into()));
    }
    Ok(())
}
fn is_reparse(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
pub(crate) fn checked_relative(root: &Path, relative: &Path) -> Result<PathBuf, DomainError> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(DomainError::InvalidScope(relative.display().to_string()));
        };
        let name_text = name
            .to_str()
            .ok_or_else(|| DomainError::InvalidScope(relative.display().to_string()))?;
        // Windows aliases and ADS must be rejected lexically even when the
        // target does not exist yet (canonicalization cannot protect that case).
        let stem = name_text
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = [
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
        ];
        if name_text.ends_with('.')
            || name_text.ends_with(' ')
            || name_text.chars().any(|c| {
                c < ' ' || matches!(c, ':' | '\\' | '/' | '<' | '>' | '"' | '|' | '?' | '*')
            })
            || reserved.contains(&stem.as_str())
        {
            return Err(DomainError::InvalidScope(relative.display().to_string()));
        }
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if is_reparse(&meta) => {
                return Err(DomainError::InvalidScope(path.display().to_string()))
            }
            Ok(_) => {
                if !fs::canonicalize(&path)?.starts_with(root) {
                    return Err(DomainError::InvalidScope(path.display().to_string()));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}
pub(crate) fn checked_profile_root_in(app_data: &Path, id: &str) -> Result<PathBuf, DomainError> {
    validate_id(id)?;
    let app_data = fs::canonicalize(app_data)?;
    checked_relative(&app_data, &Path::new("profiles").join(id))
}
pub(crate) fn read_manifest_unlocked(app_data: &Path) -> Result<ProfilesManifest, DomainError> {
    if RESET_ROOTS
        .get_or_init(Default::default)
        .lock()
        .contains(app_data)
    {
        return Err(DomainError::EditorSessionExpired);
    }
    let path = checked_relative(app_data, Path::new("profiles.json"))?;
    let manifest: ProfilesManifest = serde_json::from_str(&fs::read_to_string(path)?)?;
    validate_id(&manifest.active)?;
    let mut ids = std::collections::HashSet::new();
    for profile in &manifest.profiles {
        validate_id(&profile.id)?;
        if !ids.insert(profile.id.to_lowercase()) {
            return Err(DomainError::InvalidScope("Duplicate profile ID".into()));
        }
    }
    if !profile_exists(&manifest, &manifest.active) {
        return Err(DomainError::InvalidScope(manifest.active.clone()));
    }
    Ok(manifest)
}
fn write_manifest(app_data: &Path, manifest: &ProfilesManifest) -> Result<(), DomainError> {
    let path = checked_relative(app_data, Path::new("profiles.json"))?;
    use std::io::Write;
    let content = serde_json::to_vec_pretty(manifest)?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write(|file| {
            file.write_all(&content)?;
            file.sync_all()
        })
        .map_err(|e| DomainError::Storage(format!("Atomic manifest write failed: {e}")))?;
    Ok(())
}

/// Registry lifecycle operations use the same lock order as note transactions.
pub struct ProfileRegistry;
impl ProfileRegistry {
    /// Resolve the exact application-owned target before stopping services or
    /// deleting anything. Callers supply Tauri's current app_data_dir only.
    pub fn checked_reset_target_in(app_data: &Path) -> Result<PathBuf, DomainError> {
        if !app_data.is_absolute() || app_data.parent().is_none() {
            return Err(DomainError::InvalidScope(
                "Invalid app-data reset target".into(),
            ));
        }
        let meta = fs::symlink_metadata(app_data)?;
        if !meta.is_dir() || is_reparse(&meta) {
            return Err(DomainError::InvalidScope(
                "Invalid app-data reset target".into(),
            ));
        }
        let target = fs::canonicalize(app_data)?;
        if target.parent().is_none() {
            return Err(DomainError::InvalidScope(
                "Invalid app-data reset target".into(),
            ));
        }
        Ok(target)
    }
    /// Shared filesystem boundary for the explicit, typed all-data reset.
    /// MCP admission/runtime must already be stopped and drained by the caller.
    /// This writer waits for every note/profile transaction and excludes new
    /// ones during deletion. No lock is held by the async shutdown phase.
    pub fn reset_app_data_in(
        app_data: &Path,
        editors: &super::editors::EditorRegistry,
    ) -> Result<(), DomainError> {
        let _registry = REGISTRY.write();
        let app_data = Self::checked_reset_target_in(app_data)?;
        RESET_ROOTS
            .get_or_init(Default::default)
            .lock()
            .insert(app_data.clone());
        // The typed all-data confirmation explicitly discards unsaved drafts;
        // ordinary profile switches retain their existing clean-editor guard.
        editors.discard_sessions_for_reset();
        for entry in fs::read_dir(&app_data)? {
            let path = entry?.path();
            if path.parent() != Some(app_data.as_path()) {
                return Err(DomainError::InvalidScope(
                    "Reset entry escaped app-data".into(),
                ));
            }
            let meta = fs::symlink_metadata(&path)?;
            #[cfg(windows)]
            let directory_link = {
                use std::os::windows::fs::MetadataExt;
                is_reparse(&meta) && meta.file_attributes() & 0x10 != 0
            };
            #[cfg(not(windows))]
            let directory_link = false;
            if directory_link {
                // Remove a directory link itself; never recurse into its target.
                fs::remove_dir(path)?;
            } else if meta.is_dir() {
                if !fs::canonicalize(&path)?.starts_with(&app_data) {
                    return Err(DomainError::InvalidScope(
                        "Reset entry escaped app-data".into(),
                    ));
                }
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        Ok(())
    }
    pub fn manifest_in(app_data: &Path) -> Result<ProfilesManifest, DomainError> {
        let _registry = REGISTRY.read();
        read_manifest_unlocked(&fs::canonicalize(app_data)?)
    }
    pub fn create_in(app_data: &Path, name: &str) -> Result<ProfileMeta, DomainError> {
        validate_profile_name(name).map_err(DomainError::InvalidInput)?;
        let _registry = REGISTRY.write();
        let app_data = fs::canonicalize(app_data)?;
        let mut manifest = read_manifest_unlocked(&app_data)?;
        let base = name_to_id(name.trim());
        validate_id(&base)?;
        let id = generate_unique_id(&base, &manifest.profiles);
        let root = checked_profile_root_in(&app_data, &id)?;
        // Do not adopt files from an unregistered, previously deleted directory.
        if root.exists() {
            return Err(DomainError::InvalidScope(
                "Unregistered profile directory already exists".into(),
            ));
        }
        let lock = profile_lock(&root);
        let _profile = lock.lock();
        fs::create_dir_all(&root)?;
        let profile = ProfileMeta {
            id,
            name: name.trim().into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        manifest.profiles.push(profile.clone());
        write_manifest(&app_data, &manifest)?;
        Ok(profile)
    }
    pub fn rename_in(app_data: &Path, id: &str, name: &str) -> Result<(), DomainError> {
        validate_profile_name(name).map_err(DomainError::InvalidInput)?;
        let _registry = REGISTRY.write();
        let scope = ProfileScope::resolve_unlocked(app_data, id)?;
        let lock = profile_lock(&scope.root);
        let _profile = lock.lock();
        let mut manifest = read_manifest_unlocked(&scope.app_data)?;
        manifest
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(DomainError::ProfileDeleted)?
            .name = name.trim().into();
        write_manifest(&scope.app_data, &manifest)
    }
    pub fn switch_in(app_data: &Path, id: &str) -> Result<ProfileScope, DomainError> {
        Self::switch_with_preflight_in(app_data, id, |_| Ok(()), |_, ()| {})
    }
    /// Registry/profile locks precede the editor guard returned by `preflight`.
    /// A failed preflight or durable manifest write drops that guard unchanged.
    /// Only after persistence may `activate` invalidate sessions and update AppState.
    pub fn switch_with_preflight_in<T>(
        app_data: &Path,
        id: &str,
        preflight: impl FnOnce(&ProfileScope) -> Result<T, DomainError>,
        activate: impl FnOnce(&ProfileScope, T),
    ) -> Result<ProfileScope, DomainError> {
        let _registry = REGISTRY.write();
        let scope = ProfileScope::resolve_unlocked(app_data, id)?;
        let lock = profile_lock(&scope.root);
        let _profile = lock.lock();
        let permit = preflight(&scope)?;
        let mut manifest = read_manifest_unlocked(&scope.app_data)?;
        manifest.active = id.into();
        write_manifest(&scope.app_data, &manifest)?;
        activate(&scope, permit);
        Ok(scope)
    }
    pub fn delete_in(app_data: &Path, id: &str) -> Result<(), DomainError> {
        let _registry = REGISTRY.write();
        let scope = ProfileScope::resolve_unlocked(app_data, id)?;
        let lock = profile_lock(&scope.root);
        let _profile = lock.lock();
        let mut manifest = read_manifest_unlocked(&scope.app_data)?;
        if manifest.active == id {
            return Err(DomainError::InvalidInput(
                "Cannot delete the currently active profile. Switch to another profile first."
                    .into(),
            ));
        }
        if manifest.profiles.len() <= 1 {
            return Err(DomainError::InvalidInput(
                "Cannot delete the last remaining profile.".into(),
            ));
        }
        manifest.profiles.retain(|p| p.id != id);
        // Invalidate old scopes even if deleting the directory subsequently fails.
        write_manifest(&scope.app_data, &manifest)?;
        fs::remove_dir_all(&scope.root)?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::note_service::repository::NoteMeta;
    use crate::note_service::{Actor, MutationIntent, NoteRepository, NoteService};
    use std::fs;

    pub(crate) struct Fixture(pub PathBuf);
    impl Fixture {
        pub(crate) fn new() -> Self {
            let root = std::env::temp_dir().join(format!("lexena_scope_{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(root.join("profiles/perso")).unwrap();
            fs::create_dir_all(root.join("profiles/work")).unwrap();
            fs::write(root.join("profiles.json"), r#"{"active":"perso","profiles":[{"id":"perso","name":"Perso","createdAt":"1"},{"id":"work","name":"Work","createdAt":"2"}]}"#).unwrap();
            Self(root)
        }
        pub(crate) fn scope(&self, id: &str) -> ProfileScope {
            ProfileScope::resolve_in(&self.0, id).unwrap()
        }
        pub(crate) fn edit_registry(&self, edit: impl FnOnce(&mut serde_json::Value)) {
            let mut json =
                serde_json::from_str(&fs::read_to_string(self.0.join("profiles.json")).unwrap())
                    .unwrap();
            edit(&mut json);
            fs::write(
                self.0.join("profiles.json"),
                serde_json::to_vec(&json).unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    pub(crate) fn meta(id: &str) -> NoteMeta {
        NoteMeta {
            id: id.into(),
            title: "Original".into(),
            created_at: "1".into(),
            updated_at: "1".into(),
            favorite: false,
            folder_id: None,
            order: 0,
            deleted_at: None,
            local_only: false,
        }
    }
    pub(crate) const ID: &str = "00000000-0000-4000-8000-000000000001";

    #[test]
    fn reset_discards_dirty_editor_sessions_and_fences_queued_ui_imports() {
        use crate::note_service::editors::EditorRegistry;
        let f = Fixture::new();
        let scope = f.scope("perso");
        let editors = EditorRegistry::default();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "before".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let session = editors.session("perso");
        editors.bind_window("main", session.clone()).unwrap();
        let context = scope
            .transaction(|p| editors.register_note_locked("main", p, ID, "editor"))
            .unwrap()
            .context;
        editors.begin("main", &context).unwrap();
        assert!(matches!(
            editors.prepare_profile_switch(),
            Err(DomainError::EditorBusy)
        ));
        ProfileRegistry::reset_app_data_in(&f.0, &editors).unwrap();
        assert!(matches!(
            editors.validate_session("perso", session.session_generation),
            Err(DomainError::EditorSessionExpired)
        ));
        assert!(matches!(
            editors.validate_context("main", &context),
            Err(DomainError::EditorSessionExpired)
        ));
        assert!(editors.window_session("main").is_none());
        assert!(scope
            .transaction(|p| editors.save_note_locked(
                "main",
                p,
                &context,
                "late UI save".into(),
                "late".into(),
                uuid::Uuid::new_v4().to_string()
            ))
            .is_err());
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "late backup".into()
            },
            Actor::Backup
        )
        .is_err());
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
        // Even if a different subsystem recreates a manifest before restart,
        // neither stale scopes nor newly resolved profile transactions revive.
        fs::create_dir_all(f.0.join("profiles/perso")).unwrap();
        fs::write(
            f.0.join("profiles.json"),
            r#"{"active":"perso","profiles":[{"id":"perso","name":"Perso","createdAt":"1"}]}"#,
        )
        .unwrap();
        assert!(ProfileScope::resolve_in(&f.0, "perso").is_err());
        assert!(ProfileRegistry::create_in(&f.0, "Late").is_err());
        assert!(NoteRepository::list(&scope).is_err());
        assert!(!scope.root().join("notes").exists());
    }

    #[test]
    fn reset_target_rejects_relative_paths_files_and_the_filesystem_root() {
        let f = Fixture::new();
        assert!(ProfileRegistry::checked_reset_target_in(Path::new(".")).is_err());
        assert!(ProfileRegistry::checked_reset_target_in(&f.0.join("profiles.json")).is_err());
        let absolute = fs::canonicalize(&f.0).unwrap();
        let volume_root = absolute.ancestors().last().unwrap();
        assert!(ProfileRegistry::checked_reset_target_in(volume_root).is_err());
        assert!(f.0.join("profiles.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn failed_reset_keeps_native_sessions_fenced_until_restart() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        let scope = f.scope("perso");
        let editors = crate::note_service::editors::EditorRegistry::default();
        let session = editors.session("perso");
        fs::write(f.0.join("locked"), "synthetic locked file").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(f.0.join("locked"))
            .unwrap();
        assert!(ProfileRegistry::reset_app_data_in(&f.0, &editors).is_err());
        assert!(matches!(
            editors.validate_session("perso", session.session_generation),
            Err(DomainError::EditorSessionExpired)
        ));
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "late".into()
            },
            Actor::Ui
        )
        .is_err());
        drop(held);
        assert!(!scope.root().join("notes").exists());
    }

    #[cfg(windows)]
    #[test]
    fn reset_removes_a_directory_junction_without_erasing_its_external_target() {
        let f = Fixture::new();
        let external = Fixture::new();
        fs::write(external.0.join("sentinel"), "unchanged").unwrap();
        junction(&f.0.join("linked"), &external.0);
        ProfileRegistry::reset_app_data_in(
            &f.0,
            &crate::note_service::editors::EditorRegistry::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(external.0.join("sentinel")).unwrap(),
            "unchanged"
        );
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
    }

    #[test]
    fn explicit_work_does_not_touch_active_perso() {
        let f = Fixture::new();
        let work = f.scope("work");
        NoteService::mutate(
            &work,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "work".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        assert_eq!(NoteRepository::get(&work, ID).unwrap().content, "work");
        assert!(!f.0.join("profiles/perso/notes").exists());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &fs::read_to_string(f.0.join("profiles.json")).unwrap()
            )
            .unwrap()["active"],
            "perso"
        );
    }
    #[test]
    fn a_purged_note_directory_is_not_found_but_corrupt_metadata_is_storage_error() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let directory = scope.root().join("notes").join(ID);
        fs::create_dir_all(&directory).unwrap();
        assert!(matches!(
            NoteRepository::get(&scope, ID),
            Err(DomainError::NotFound(_))
        ));
        fs::write(directory.join("note.json"), "{corrupt").unwrap();
        assert!(matches!(
            NoteRepository::get(&scope, ID),
            Err(DomainError::Storage(_))
        ));
    }
    #[test]
    fn rename_keeps_scope_id() {
        let f = Fixture::new();
        let scope = f.scope("work");
        ProfileRegistry::rename_in(&f.0, "work", "Business").unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "retained".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        assert_eq!(scope.local_id(), "work");
        assert_eq!(NoteRepository::get(&scope, ID).unwrap().content, "retained");
    }
    #[test]
    fn deleted_profile_cannot_commit() {
        let f = Fixture::new();
        let scope = f.scope("work");
        f.edit_registry(|j| {
            j["profiles"].as_array_mut().unwrap().pop();
        });
        let err = NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "no".into(),
            },
            Actor::Ui,
        )
        .unwrap_err();
        assert!(matches!(err, DomainError::ProfileDeleted));
        assert!(!f.0.join("profiles/work/notes").exists());
    }
    #[test]
    fn reject_unknown_profile() {
        let f = Fixture::new();
        assert!(matches!(
            ProfileScope::resolve_in(&f.0, "ghost"),
            Err(DomainError::InvalidScope(_))
        ));
        assert!(!f.0.join("profiles/ghost").exists());
    }
    #[test]
    fn reject_path_traversal_and_reparse_escape() {
        let f = Fixture::new();
        for id in [
            "../outside",
            "..",
            "C:\\outside",
            "work/../perso",
            "work:stream",
            "work.",
        ] {
            assert!(
                matches!(
                    ProfileScope::resolve_in(&f.0, id),
                    Err(DomainError::InvalidScope(_))
                ),
                "{id}"
            );
        }
        let scope = f.scope("work");
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta("../../outside"),
                content: "no".into()
            },
            Actor::Ui
        )
        .is_err());
        assert!(!f.0.join("outside").exists());
    }
    #[test]
    fn switch_does_not_retarget_a_captured_request() {
        let f = Fixture::new();
        let captured = f.scope("perso");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            NoteService::mutate(
                &captured,
                MutationIntent::ImportNote {
                    meta: meta(ID),
                    content: "captured perso".into(),
                },
                Actor::Ui,
            )
            .unwrap();
        });
        ready_rx.recv().unwrap();
        ProfileRegistry::switch_in(&f.0, "work").unwrap();
        go_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(
            NoteRepository::get(&f.scope("perso"), ID).unwrap().content,
            "captured perso"
        );
        assert!(NoteRepository::list(&f.scope("work")).unwrap().is_empty());
    }
    #[test]
    fn deletion_waits_for_transaction_and_invalidates_old_scope() {
        let f = Fixture::new();
        let captured = f.scope("work");
        let stale = captured.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            NoteService::transaction(&captured, |p| {
                entered_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                NoteService::mutate_locked(
                    p,
                    MutationIntent::ImportNote {
                        meta: meta(ID),
                        content: "before delete".into(),
                    },
                    Actor::Ui,
                )?;
                Ok(())
            })
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let root = f.0.clone();
        let (deleted_tx, deleted_rx) = std::sync::mpsc::channel();
        let deleter = std::thread::spawn(move || {
            ProfileRegistry::delete_in(&root, "work").unwrap();
            deleted_tx.send(()).unwrap();
        });
        assert!(deleted_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        finish_tx.send(()).unwrap();
        writer.join().unwrap();
        deleted_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        deleter.join().unwrap();
        assert!(!f.0.join("profiles/work").exists());
        assert!(matches!(
            NoteService::mutate(
                &stale,
                MutationIntent::ImportNote {
                    meta: meta(ID),
                    content: "late".into()
                },
                Actor::Ui
            ),
            Err(DomainError::ProfileDeleted)
        ));
        assert!(!f.0.join("profiles/work").exists());
    }
    #[test]
    fn recreated_profile_does_not_revive_old_scope() {
        let f = Fixture::new();
        let stale = f.scope("work");
        ProfileRegistry::delete_in(&f.0, "work").unwrap();
        let recreated = ProfileRegistry::create_in(&f.0, "Work").unwrap();
        assert_eq!(recreated.id, "work");
        assert!(matches!(
            NoteRepository::list(&stale),
            Err(DomainError::ProfileDeleted)
        ));
        assert!(NoteRepository::list(&f.scope("work")).unwrap().is_empty());
    }
    #[test]
    fn malformed_manifest_ids_are_rejected_before_any_profile_io() {
        let f = Fixture::new();
        f.edit_registry(|j| {
            j["active"] = "../outside".into();
            j["profiles"][0]["id"] = "../outside".into();
        });
        assert!(matches!(
            ProfileRegistry::manifest_in(&f.0),
            Err(DomainError::InvalidScope(_))
        ));
        assert!(!f.0.join("outside").exists());
    }
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "junction creation failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    #[cfg(windows)]
    #[test]
    fn reject_windows_junction_at_profile_and_nested_note_paths() {
        let f = Fixture::new();
        let external = Fixture::new();
        fs::write(external.0.join("sentinel"), "unchanged").unwrap();
        fs::remove_dir(f.0.join("profiles/work")).unwrap();
        junction(&f.0.join("profiles/work"), &external.0);
        assert!(matches!(
            ProfileScope::resolve_in(&f.0, "work"),
            Err(DomainError::InvalidScope(_))
        ));
        fs::remove_dir(f.0.join("profiles/work")).unwrap();
        fs::create_dir(f.0.join("profiles/work")).unwrap();
        let scope = f.scope("work");
        junction(&scope.root().join("notes"), &external.0);
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "escape".into()
            },
            Actor::Ui
        )
        .is_err());
        assert!(!external.0.join(ID).exists());
        fs::remove_dir(scope.root().join("notes")).unwrap();
        fs::create_dir(scope.root().join("notes")).unwrap();
        junction(&scope.root().join("notes").join(ID), &external.0);
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "escape".into()
            },
            Actor::Ui
        )
        .is_err());
        assert_eq!(
            fs::read_to_string(external.0.join("sentinel")).unwrap(),
            "unchanged"
        );
        assert!(!external.0.join("note.json").exists());
        fs::remove_dir(scope.root().join("notes").join(ID)).unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "original".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let content = scope.root().join("notes").join(ID).join("content.html");
        fs::remove_file(&content).unwrap();
        junction(&content, &external.0);
        assert!(matches!(
            NoteRepository::get(&scope, ID),
            Err(DomainError::InvalidScope(_))
        ));
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::UpdateNote {
                id: ID.into(),
                content: "bad".into(),
                title: "bad".into()
            },
            Actor::Ui
        )
        .is_err());
        fs::remove_dir(&content).unwrap();
        let metadata = scope.root().join("notes").join(ID).join("note.json");
        fs::remove_file(&metadata).unwrap();
        junction(&metadata, &external.0);
        assert!(NoteRepository::get(&scope, ID).is_err());
        fs::remove_dir(&metadata).unwrap();
        assert_eq!(
            fs::read_to_string(external.0.join("sentinel")).unwrap(),
            "unchanged"
        );
    }
    #[test]
    fn locked_paths_reject_windows_aliases_and_alternate_streams() {
        let f = Fixture::new();
        let scope = f.scope("work");
        NoteService::transaction(&scope, |p| {
            for path in [
                "notes/file:stream",
                "notes/trailing.",
                "notes/trailing ",
                "notes/CON.txt",
                "../outside",
            ] {
                assert!(
                    matches!(p.path(Path::new(path)), Err(DomainError::InvalidScope(_))),
                    "{path}"
                );
            }
            Ok(())
        })
        .unwrap();
        assert!(!scope.root().join("notes").exists());
    }
    #[test]
    fn failed_switch_preflight_preserves_manifest_and_activation_state() {
        let f = Fixture::new();
        let mut active = "perso".to_string();
        let mut session_generation = 1;
        let result = ProfileRegistry::switch_with_preflight_in(
            &f.0,
            "work",
            |_| Err::<(), _>(DomainError::EditorBusy),
            |s, _| {
                active = s.local_id().into();
                session_generation += 1;
            },
        );
        assert!(matches!(result, Err(DomainError::EditorBusy)));
        assert_eq!(ProfileRegistry::manifest_in(&f.0).unwrap().active, "perso");
        assert_eq!(active, "perso");
        assert_eq!(session_generation, 1);
        ProfileRegistry::switch_with_preflight_in(
            &f.0,
            "work",
            |_| Ok(()),
            |s, _| {
                active = s.local_id().into();
                session_generation += 1;
            },
        )
        .unwrap();
        assert_eq!(ProfileRegistry::manifest_in(&f.0).unwrap().active, "work");
        assert_eq!(active, "work");
        assert_eq!(session_generation, 2);
    }
    #[cfg(windows)]
    #[test]
    fn failed_switch_manifest_replace_drops_preflight_without_activating() {
        use std::os::windows::fs::OpenOptionsExt;
        let f = Fixture::new();
        let mut active = "perso".to_string();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(f.0.join("profiles.json"))
            .unwrap();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct Permit(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Permit {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        assert!(ProfileRegistry::switch_with_preflight_in(
            &f.0,
            "work",
            |_| Ok(Permit(dropped.clone())),
            |s, _| active = s.local_id().into()
        )
        .is_err());
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(active, "perso");
        assert_eq!(ProfileRegistry::manifest_in(&f.0).unwrap().active, "perso");
        drop(file);
    }
}
