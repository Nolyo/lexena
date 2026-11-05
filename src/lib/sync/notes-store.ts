import { invoke } from "@tauri-apps/api/core";
import type { CloudUserNoteRow, LocalNoteMeta } from "./types";
import { mapNoteFromCloud } from "./mapping";
import { isNoteSyncable } from "./note-size";
import { getQueueSession } from "./queue";
import { requestOutboxDrain } from "./local-outbox";

// Native journal writes are the sole producer of note sync intentions.
export async function listNotes(): Promise<LocalNoteMeta[]> { return invoke("list_notes"); }
export async function readNote(id: string): Promise<{ meta: LocalNoteMeta; content: string }> { return invoke("read_note", { id }); }
export async function scanOversizedNoteCount(): Promise<number> {
  let count = 0;
  for (const meta of await listNotes()) {
    if (meta.deletedAt || meta.localOnly) continue;
    try { if (!isNoteSyncable((await readNote(meta.id)).content)) count++; } catch { /* unreadable local note */ }
  }
  return count;
}
async function mutate<T>(command: string, args: Record<string, unknown>): Promise<T> {
  const result = await invoke<T>(command, { ...args, session: await getQueueSession() });
  requestOutboxDrain();
  return result;
}
export function createNoteSynced(folderId: string | null): Promise<LocalNoteMeta> { return mutate("create_note", { folderId }); }
export function updateNoteSynced(id: string, content: string, title: string): Promise<LocalNoteMeta> { return mutate("update_note", { id, content, title }); }
export function deleteNoteSynced(id: string): Promise<void> { cancelNoteUpdatePush(id); return mutate("delete_note", { id }); }
export function toggleNoteFavoriteSynced(id: string): Promise<LocalNoteMeta> { return mutate("toggle_note_favorite", { id }); }
export function moveNoteToFolderSynced(noteId: string, folderId: string | null): Promise<LocalNoteMeta> { return mutate("move_note_to_folder", { noteId, folderId }); }
export function setNoteLocalOnlySynced(id: string, localOnly: boolean): Promise<LocalNoteMeta> { cancelNoteUpdatePush(id); return mutate("set_note_local_only", { id, localOnly }); }
export async function pushNoteUpdate(_meta: LocalNoteMeta, _content: string): Promise<void> { requestOutboxDrain(); }

const updateNoteDebounceMap = new Map<string, ReturnType<typeof setTimeout>>();
export const UPDATE_NOTE_PUSH_DEBOUNCE_MS = 2_000;
// Delayed work only wakes the main consumer. It never captures old HTML or
// resolves an active profile after the renderer session has changed.
export function scheduleNoteUpdatePushFromDisk(id: string, delayMs: number): void {
  cancelNoteUpdatePush(id);
  updateNoteDebounceMap.set(id, setTimeout(() => { updateNoteDebounceMap.delete(id); requestOutboxDrain(); }, delayMs));
}
export function scheduleNoteUpdatePush(id: string, _meta: LocalNoteMeta, _content: string, delayMs: number): void { scheduleNoteUpdatePushFromDisk(id, delayMs); }
export function cancelNoteUpdatePush(id: string): void { const timer = updateNoteDebounceMap.get(id); if (timer) clearTimeout(timer); updateNoteDebounceMap.delete(id); }
export async function flushPendingNoteUpdates(): Promise<void> {
  const pending = updateNoteDebounceMap.size;
  for (const timer of updateNoteDebounceMap.values()) clearTimeout(timer);
  updateNoteDebounceMap.clear();
  if (pending) requestOutboxDrain();
}
export interface RemoteApplyResult { applied: boolean; deferred: boolean }
export async function applyRemoteNote(row: CloudUserNoteRow): Promise<RemoteApplyResult> {
  const session = await getQueueSession();
  const { meta, content } = mapNoteFromCloud(row);
  return invoke("apply_sync_remote_note", { session, meta, content });
}
