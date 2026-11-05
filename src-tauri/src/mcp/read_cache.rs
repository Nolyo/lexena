//! Bounded derived read data. The profile transaction protects the snapshot;
//! authorization is previewed before construction and retained again for output.
use crate::note_service::{
    journal::{Revision, RevisionStore},
    repository::NoteMeta,
    DomainError, LockedProfile, NoteRepository,
};
use parking_lot::Mutex;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};
use unicode_normalization::UnicodeNormalization;
pub const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
pub fn normalized(s: &str) -> String {
    s.nfkc().flat_map(char::to_lowercase).collect()
}
pub fn bounded(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].into()
}
#[derive(Clone, PartialEq, Eq, Hash)]
struct Identity {
    root: PathBuf,
    created_at: String,
    private: bool,
}
pub struct IndexedNote {
    pub meta: NoteMeta,
    pub revision: Revision,
    pub excerpt: String,
    pub search_text: Option<String>,
}
pub struct ReadIndex {
    pub collection: Revision,
    pub notes: Vec<Arc<IndexedNote>>,
    revisions: BTreeMap<String, Revision>,
    bytes: usize,
}
#[derive(Default)]
struct State {
    entries: HashMap<Identity, (u64, Arc<ReadIndex>)>,
    clock: u64,
    bytes: usize,
}
#[derive(Default)]
pub struct ReadCache {
    state: Mutex<State>,
    // Only one cold/rebuild allocation at a time, while warm hits remain free.
    builder: Mutex<()>,
    #[cfg(test)]
    observer: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
}
impl ReadCache {
    #[cfg(test)]
    pub(super) fn observe(&self, callback: Arc<dyn Fn(&str) + Send + Sync>) {
        *self.observer.lock() = Some(callback);
    }
    fn find(&self, key: &Identity) -> Option<Arc<ReadIndex>> {
        let mut state = self.state.lock();
        state.clock = state.clock.wrapping_add(1);
        let clock = state.clock;
        state.entries.get_mut(key).map(|(used, index)| {
            *used = clock;
            index.clone()
        })
    }
    pub fn get_locked(
        &self,
        p: &LockedProfile<'_>,
        private: bool,
    ) -> Result<Arc<ReadIndex>, DomainError> {
        let key = Identity {
            root: p.scope().root().into(),
            created_at: p.scope().created_at().into(),
            private,
        };
        let (collection, mut revisions) = RevisionStore::note_snapshot_locked(p, &[])?;
        let previous = self.find(&key);
        if let Some(index) = &previous {
            if index.collection == collection {
                return Ok(index.clone());
            }
        }
        let _building = self.builder.lock();
        let mut by_id: BTreeMap<String, Arc<IndexedNote>> = previous
            .as_ref()
            .map(|index| {
                index
                    .notes
                    .iter()
                    .map(|n| (n.meta.id.clone(), n.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut changed = Vec::new();
        if let Some(old) = &previous {
            for (id, revision) in &revisions {
                if old.revisions.get(id) != Some(revision) {
                    changed.push((id.clone(), None));
                }
            }
            by_id.retain(|id, _| revisions.contains_key(id));
        } else {
            let all = NoteRepository::list_locked(p)?;
            let ids = all.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
            let snapshot = RevisionStore::note_snapshot_locked(p, &ids)?;
            revisions = snapshot.1;
            changed = all
                .into_iter()
                .map(|meta| (meta.id.clone(), Some(meta)))
                .collect();
        }
        let identity_bytes = key.root.capacity() + key.created_at.capacity() + 1024;
        let mut estimated = identity_bytes + estimate(&by_id, &revisions);
        for (id, meta) in changed {
            let meta = match meta
                .map(Ok)
                .unwrap_or_else(|| NoteRepository::metadata_locked(p, &id))
            {
                Ok(meta) => meta,
                Err(DomainError::NotFound(_)) => {
                    if let Some(old) = by_id.remove(&id) {
                        estimated = estimated.saturating_sub(entry_bytes(&old));
                    }
                    continue;
                }
                Err(e) => return Err(e),
            };
            if meta.deleted_at.is_some() || meta.local_only && !private {
                if let Some(old) = by_id.remove(&id) {
                    estimated = estimated.saturating_sub(entry_bytes(&old));
                }
                continue;
            }
            #[cfg(test)]
            if let Some(observer) = self.observer.lock().clone() {
                observer(&id);
            }
            let html = NoteRepository::content_locked(p, &id)?;
            let plain = crate::note_service::content::plain_text(&html);
            let search_text = normalized(&format!("{}\n{}", meta.title, plain));
            let revision = revisions
                .get(&id)
                .cloned()
                .ok_or(DomainError::RecoveryRequired)?;
            if let Some(old) = by_id.remove(&id) {
                estimated = estimated.saturating_sub(entry_bytes(&old));
            }
            let mut next = IndexedNote {
                meta,
                revision,
                excerpt: bounded(&plain, 320),
                search_text: Some(search_text),
            };
            // Never make listing depend on keeping every full search body in
            // RAM. Uncached bodies are searched from the checked repository.
            if estimated.saturating_add(entry_bytes(&next)) > MAX_CACHE_BYTES {
                next.search_text = None;
            }
            estimated = estimated.saturating_add(entry_bytes(&next));
            by_id.insert(id, Arc::new(next));
            if estimated > MAX_CACHE_BYTES {
                return Err(DomainError::InvalidInput(
                    "Read index exceeds bounded cache capacity".into(),
                ));
            }
        }
        let bytes = identity_bytes + estimate(&by_id, &revisions);
        if bytes > MAX_CACHE_BYTES {
            return Err(DomainError::InvalidInput(
                "Read index exceeds bounded cache capacity".into(),
            ));
        }
        let mut notes = by_id.into_values().collect::<Vec<_>>();
        notes.sort_by(|a, b| {
            b.meta
                .updated_at
                .cmp(&a.meta.updated_at)
                .then(a.meta.id.cmp(&b.meta.id))
        });
        let index = Arc::new(ReadIndex {
            collection,
            notes,
            revisions,
            bytes,
        });
        let mut state = self.state.lock();
        if let Some((_, old)) = state.entries.remove(&key) {
            state.bytes = state.bytes.saturating_sub(old.bytes);
        }
        while state.bytes + bytes > MAX_CACHE_BYTES {
            let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, value)| value.0)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some((_, old)) = state.entries.remove(&oldest) {
                state.bytes = state.bytes.saturating_sub(old.bytes);
            }
        }
        state.clock = state.clock.wrapping_add(1);
        let clock = state.clock;
        state.bytes += bytes;
        state.entries.insert(key, (clock, index.clone()));
        Ok(index)
    }
}
fn estimate(
    notes: &BTreeMap<String, Arc<IndexedNote>>,
    revisions: &BTreeMap<String, Revision>,
) -> usize {
    revisions
        .iter()
        .map(|(id, r)| id.len() + r.as_str().len() + 96)
        .sum::<usize>()
        + notes.values().map(|n| entry_bytes(n)).sum::<usize>()
}
fn entry_bytes(n: &IndexedNote) -> usize {
    n.meta.id.capacity()
        + n.meta.title.capacity()
        + n.meta.created_at.capacity()
        + n.meta.updated_at.capacity()
        + n.meta.folder_id.as_ref().map_or(0, String::capacity)
        + n.search_text.as_ref().map_or(0, String::capacity)
        + n.excerpt.capacity()
        + 512
}
