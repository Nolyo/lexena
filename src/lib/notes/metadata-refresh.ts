import { invoke } from '@tauri-apps/api/core';
import type { NoteMeta } from '@/hooks/useNotes';

export interface MetadataSession {
  profile_id: string;
  session_generation: number;
}
export interface MetadataChange {
  profile_id: string;
  session_generation?: number;
  note_id: string;
}

/** One serialized reader per subscriber. IDs changed during a read remain queued
 * for a fresh read; the older result is never applied for those IDs. */
export function createScopedMetadataRefresh<T>(options: {
  session: MetadataSession;
  currentSession: () => Promise<MetadataSession>;
  fetch: (ids: string[]) => Promise<T[]>;
  apply: (ids: string[], values: T[]) => void;
  onError: (error: unknown) => void;
}) {
  const pending = new Set<string>();
  let disposed = false;
  let running = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const dispose = () => {
    disposed = true;
    pending.clear();
    clearTimeout(timer);
  };
  const isCurrent = async () => {
    if (disposed) return false;
    const current = await options.currentSession();
    if (disposed) return false;
    if (current.profile_id !== options.session.profile_id
      || current.session_generation !== options.session.session_generation) {
      dispose();
      return false;
    }
    return true;
  };
  const drain = async () => {
    timer = undefined;
    if (disposed || running) return;
    running = true;
    try {
      while (!disposed && pending.size) {
        const ids = [...pending].slice(0, 100);
        ids.forEach(id => pending.delete(id));
        try {
          if (!await isCurrent()) return;
          const result = await options.fetch(ids);
          if (!await isCurrent()) return;
          // apply() merges by these IDs and ignores any other values returned.
          const unchanged = ids.filter(id => !pending.has(id));
          if (unchanged.length) options.apply(unchanged, result);
        } catch (error) {
          if (!disposed) options.onError(error);
        }
      }
    } finally {
      running = false;
    }
  };
  return {
    enqueue(change: MetadataChange) {
      if (disposed || change.profile_id !== options.session.profile_id
        || (change.session_generation !== undefined && change.session_generation !== options.session.session_generation)
        || !change.note_id) return;
      pending.add(change.note_id);
      if (!running && timer === undefined) timer = setTimeout(() => { void drain(); }, 25);
    },
    dispose,
  };
}

export function createNoteMetadataRefresh(
  session: MetadataSession,
  apply: (ids: string[], notes: NoteMeta[]) => void,
  onError: (error: unknown) => void,
) {
  return createScopedMetadataRefresh({
    session,
    currentSession: () => invoke<MetadataSession>('get_profile_session'),
    fetch: noteIds => invoke<NoteMeta[]>('read_note_metadata_batch', { session, noteIds }),
    apply,
    onError,
  });
}

/** Missing IDs are authoritative deletions. Only requested IDs may change. */
export function mergeNoteMetadata(previous: NoteMeta[], ids: string[], values: NoteMeta[]): NoteMeta[] {
  const requested = new Set(ids);
  const fresh = new Map(values.filter(note => requested.has(note.id) && !note.deletedAt).map(note => [note.id, note]));
  const merged = previous.flatMap(note => {
    if (!requested.has(note.id)) return [note];
    const replacement = fresh.get(note.id);
    fresh.delete(note.id);
    return replacement ? [replacement] : [];
  });
  return [...merged, ...fresh.values()].sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}
