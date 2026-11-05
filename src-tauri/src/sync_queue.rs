//! Atomic queue snapshots. Callers provide a path checked by a locked ProfileScope.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueSnapshot {
    pub queue: Vec<Value>,
    pub dead_letters: Vec<Value>,
    pub imported_outbox_sequence: u64,
    pub revision: String,
    #[serde(default)]
    pub protected_entities: Vec<Value>,
}
impl Default for QueueSnapshot {
    fn default() -> Self {
        Self {
            queue: vec![],
            dead_letters: vec![],
            imported_outbox_sequence: 0,
            revision: "absent-v1".into(),
            protected_entities: vec![],
        }
    }
}

static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
fn lock_for(path: &Path) -> Result<Arc<Mutex<()>>, String> {
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "SYNC_QUEUE_LOCK".to_string())?;
    Ok(locks.entry(path.to_path_buf()).or_default().clone())
}

fn load_unlocked(path: &Path) -> Result<QueueSnapshot, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(QueueSnapshot::default())
        }
        Err(_) => return Err("SYNC_QUEUE_READ_FAILED".into()),
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "SYNC_QUEUE_CORRUPT".to_string())?;
    let object = value.as_object().ok_or("SYNC_QUEUE_CORRUPT")?;
    if object
        .get("_mcp_format")
        .is_some_and(|version| version.as_u64() != Some(1))
    {
        return Err("SYNC_QUEUE_FORMAT_UNSUPPORTED".into());
    }
    let array = |key: &str| -> Result<Vec<Value>, String> {
        match object.get(key) {
            None => Ok(vec![]),
            Some(value) => value
                .as_array()
                .cloned()
                .ok_or_else(|| "SYNC_QUEUE_CORRUPT".into()),
        }
    };
    let imported_outbox_sequence = match object.get("_mcp_imported_outbox_sequence") {
        None => 0,
        Some(value) => value.as_u64().ok_or("SYNC_QUEUE_CORRUPT")?,
    };
    let revision = match object.get("_mcp_revision") {
        None => "legacy-v1".into(),
        Some(value) => value
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or("SYNC_QUEUE_CORRUPT")?
            .to_string(),
    };
    Ok(QueueSnapshot {
        queue: array("queue")?,
        dead_letters: array("dead-letters")?,
        imported_outbox_sequence,
        revision,
        protected_entities: array("_mcp_protected_entities")?,
    })
}

pub fn load_snapshot(path: &Path) -> Result<QueueSnapshot, String> {
    let lock = lock_for(path)?;
    let _guard = lock.lock().map_err(|_| "SYNC_QUEUE_LOCK".to_string())?;
    load_unlocked(path)
}

pub fn save_snapshot(
    path: &Path,
    mut snapshot: QueueSnapshot,
    expected_revision: &str,
) -> Result<QueueSnapshot, String> {
    let lock = lock_for(path)?;
    let _guard = lock.lock().map_err(|_| "SYNC_QUEUE_LOCK".to_string())?;
    let current = load_unlocked(path)?;
    if current.revision != expected_revision {
        return Err("SYNC_QUEUE_REVISION_CONFLICT".into());
    }
    if snapshot.imported_outbox_sequence < current.imported_outbox_sequence {
        return Err("SYNC_QUEUE_WATERMARK_REGRESSION".into());
    }
    snapshot.revision = uuid::Uuid::new_v4().to_string();
    let disk = serde_json::json!({
        "queue": snapshot.queue,
        "dead-letters": snapshot.dead_letters,
        "_mcp_format": 1,
        "_mcp_imported_outbox_sequence": snapshot.imported_outbox_sequence,
        "_mcp_revision": snapshot.revision,
        "_mcp_protected_entities": snapshot.protected_entities,
    });
    let bytes = serde_json::to_vec(&disk).map_err(|_| "SYNC_QUEUE_SERIALIZE".to_string())?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write(|file| {
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .map_err(|_| "SYNC_QUEUE_SAVE_FAILED".to_string())?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn legacy_queue_and_dead_letters_survive_migration_without_a_read_side_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-queue.json");
        let legacy = r#"{"queue":[{"id":"pending","operation":{"kind":"note-delete","id":"note"}}],"dead-letters":[{"id":"failed"}]}"#;
        fs::write(&path, legacy).unwrap();
        let snapshot = load_snapshot(&path).unwrap();
        assert_eq!(snapshot.queue[0]["id"], "pending");
        assert_eq!(snapshot.dead_letters[0]["id"], "failed");
        assert_eq!(snapshot.imported_outbox_sequence, 0);
        assert_eq!(fs::read_to_string(&path).unwrap(), legacy);
        save_snapshot(&path, snapshot.clone(), &snapshot.revision).unwrap();
        assert_eq!(load_snapshot(&path).unwrap().queue[0]["id"], "pending");
    }

    #[test]
    fn imported_watermark_survives_entry_consumption_and_repeated_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-queue.json");
        let mut snapshot = load_snapshot(&path).unwrap();
        snapshot.queue.push(json!({"id":"from-outbox-7"}));
        snapshot.imported_outbox_sequence = 7;
        snapshot
            .protected_entities
            .push(json!({"kind":"note","id":"blocked","reason":"too_large","sequence":7}));
        let revision = snapshot.revision.clone();
        let mut imported = save_snapshot(&path, snapshot, &revision).unwrap();
        imported.queue.clear();
        let revision = imported.revision.clone();
        save_snapshot(&path, imported, &revision).unwrap();
        for _ in 0..2 {
            let restored = load_snapshot(&path).unwrap();
            assert!(restored.queue.is_empty());
            assert_eq!(restored.imported_outbox_sequence, 7);
            assert_eq!(restored.protected_entities[0]["id"], "blocked");
        }
    }

    #[test]
    fn stale_snapshot_cannot_replace_newer_entries_or_decrease_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-queue.json");
        let stale = load_snapshot(&path).unwrap();
        let mut first = stale.clone();
        first.queue.push(json!({"id":"keep"}));
        first.imported_outbox_sequence = 5;
        let mut fresh = save_snapshot(&path, first, &stale.revision).unwrap();
        assert!(save_snapshot(&path, stale.clone(), &stale.revision).is_err());
        fresh.imported_outbox_sequence = 4;
        assert!(save_snapshot(&path, fresh.clone(), &fresh.revision).is_err());
        assert_eq!(load_snapshot(&path).unwrap().queue[0]["id"], "keep");
        assert_eq!(load_snapshot(&path).unwrap().imported_outbox_sequence, 5);
    }

    #[test]
    fn moving_to_dead_letters_is_a_single_persisted_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-queue.json");
        let mut snapshot = load_snapshot(&path).unwrap();
        snapshot
            .queue
            .push(json!({"id":"entry","operation":{"kind":"folder-delete","id":"folder"}}));
        let revision = snapshot.revision.clone();
        let mut saved = save_snapshot(&path, snapshot, &revision).unwrap();
        let entry = saved.queue.remove(0);
        saved.dead_letters.push(entry);
        let revision = saved.revision.clone();
        save_snapshot(&path, saved, &revision).unwrap();
        let restored = load_snapshot(&path).unwrap();
        assert!(restored.queue.is_empty());
        assert_eq!(restored.dead_letters[0]["id"], "entry");
    }

    #[test]
    fn corrupt_or_future_queue_data_is_not_overwritten_with_an_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-queue.json");
        for raw in [
            "broken JSON",
            r#"{"queue":{},"dead-letters":[]}"#,
            r#"{"queue":[],"_mcp_format":99}"#,
        ] {
            fs::write(&path, raw).unwrap();
            assert!(load_snapshot(&path).is_err());
            assert!(save_snapshot(&path, QueueSnapshot::default(), "absent-v1").is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn concurrent_compare_and_save_has_one_winner_without_lost_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("sync-queue.json"));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|id| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut snapshot = load_snapshot(&path).unwrap();
                    snapshot.queue.push(json!({"id":id}));
                    let revision = snapshot.revision.clone();
                    barrier.wait();
                    save_snapshot(&path, snapshot, &revision)
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(load_snapshot(&path).unwrap().queue.len(), 1);
    }
}
