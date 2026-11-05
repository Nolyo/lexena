//! Minimal activity metadata; note content and searches never enter this schema.
use lexena_mcp_ipc::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Mutex};
use uuid::Uuid;

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditTool {
    GetContext,
    ListProfiles,
    ListFolders,
    ListNotes,
    SearchNotes,
    GetNote,
    CreateNote,
    AppendNote,
    EditNote,
    ReplaceNote,
    MoveNote,
    DeleteNote,
    RestoreNote,
    ListHistory,
    CreateFolder,
    RenameFolder,
    DeleteFolder,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuditResult {
    Ok,
    Denied,
    Conflict,
    Cancelled,
    Failed,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub connection_id: Uuid,
    pub tool: AuditTool,
    pub profile_id: Option<String>,
    pub object_id: Option<String>,
    pub result: AuditResult,
    pub duration_ms: u64,
    pub operation_id: Option<Uuid>,
}
pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}
impl AuditLog {
    pub fn new(app_data: &std::path::Path) -> Self {
        Self {
            path: app_data.join("mcp/audit.json"),
            lock: Mutex::new(()),
        }
    }
    fn read_unlocked(&self) -> Result<Vec<AuditEntry>> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let bytes = std::fs::read(&self.path)?;
        if bytes.len() > 10 * 1024 * 1024 {
            return Err(Error::Frame);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Frame)
    }
    pub fn list(&self) -> Result<Vec<AuditEntry>> {
        let _lock = self.lock.lock().map_err(|_| Error::Denied)?;
        let mut entries = self.read_unlocked()?;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
        entries.retain(|entry| entry.timestamp >= cutoff);
        Ok(entries)
    }
    pub fn append(&self, entry: AuditEntry) -> Result<()> {
        let _lock = self.lock.lock().map_err(|_| Error::Denied)?;
        // IDs only. Never let an untrusted tool argument become a log message.
        if entry
            .profile_id
            .as_deref()
            .is_some_and(|id| !safe_entity_id(id))
            || entry
                .object_id
                .as_deref()
                .is_some_and(|id| !safe_entity_id(id))
        {
            return Err(Error::Denied);
        }
        let mut entries = self.read_unlocked()?;
        entries.push(entry);
        let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
        entries.retain(|entry| entry.timestamp >= cutoff);
        while serde_json::to_vec(&entries)
            .map_err(|_| Error::Frame)?
            .len()
            > 10 * 1024 * 1024
        {
            entries.remove(0);
        }
        lexena_mcp_ipc::write_private_json(&self.path, &entries)
    }
}

/// Same bounded component grammar as the domain's legacy ID validator; this
/// independent metadata module never resolves paths or accesses entity content.
fn safe_entity_id(id: &str) -> bool {
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    !id.is_empty()
        && id.len() <= 255
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        && !reserved.contains(&id.to_ascii_uppercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn safe_legacy_entity_ids_are_audited_but_content_and_unsafe_ids_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let audit = AuditLog::new(dir.path());
        let value = serde_json::json!({"timestamp":chrono::Utc::now(),"connection_id":Uuid::new_v4(),"tool":"get_note","profile_id":"équipe_legacy","object_id":"legacy-note_123","result":"OK","duration_ms":1,"operation_id":null});
        let parsed = serde_json::from_value::<AuditEntry>(value.clone());
        assert!(
            parsed.is_ok(),
            "safe legacy note IDs must not require UUID syntax"
        );
        audit.append(parsed.unwrap()).unwrap();
        for invalid in [
            "",
            "..",
            "../other",
            "note:stream",
            "CON",
            "a b",
            "<p>private body</p>",
            "private body\n",
        ] {
            let mut invalid_value = value.clone();
            invalid_value["object_id"] = serde_json::json!(invalid);
            assert!(audit
                .append(serde_json::from_value(invalid_value).unwrap())
                .is_err());
        }
        let mut extra = value.clone();
        extra["content"] = serde_json::json!("private body");
        assert!(serde_json::from_value::<AuditEntry>(extra).is_err());
        let stored = std::fs::read_to_string(dir.path().join("mcp/audit.json")).unwrap();
        assert!(stored.contains("legacy-note_123"));
        assert!(!stored.contains("private body"));
        assert!(!stored.contains("content"));
        assert_eq!(audit.list().unwrap().len(), 1);
    }
    #[test]
    fn retention_drops_old_metadata_and_reloads_durable_events() {
        let dir = tempfile::tempdir().unwrap();
        let audit = AuditLog::new(dir.path());
        let mut event = AuditEntry {
            timestamp: chrono::Utc::now() - chrono::Duration::days(31),
            connection_id: Uuid::new_v4(),
            tool: AuditTool::GetNote,
            profile_id: Some("work".into()),
            object_id: Some(Uuid::new_v4().to_string()),
            result: AuditResult::Ok,
            duration_ms: 10,
            operation_id: None,
        };
        audit.append(event.clone()).unwrap();
        assert!(audit.list().unwrap().is_empty());
        event.timestamp = chrono::Utc::now();
        audit.append(event).unwrap();
        assert_eq!(AuditLog::new(dir.path()).list().unwrap().len(), 1);
    }
}
