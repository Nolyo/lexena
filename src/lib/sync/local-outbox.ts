import { invoke } from "@tauri-apps/api/core";
import { createMutex } from "./_mutex";
import { getImportedOutboxSequence, getQueueSession, importOutboxEntries, loadSnapshot, operationEntityKey, type ProtectedEntity } from "./queue";
import { isSyncActive } from "./sync-gate";
import { mapFolderToCloud, mapNoteToCloud } from "./mapping";
import { shouldPushNote } from "./note-push-gate";
import { isNoteSyncable } from "./note-size";
import type { LocalFolderMeta, LocalNoteMeta, SyncOperation, SyncQueueEntry } from "./types";

interface ResolvedEntity { kind: "note" | "folder"; id: string; sequence: number; deleted: boolean; cloud_delete: boolean; seed?: boolean; note?: { meta: LocalNoteMeta; content: string } | null; folder?: LocalFolderMeta | null }
interface ResolvedBatch { profile_id: string; through_sequence: number; entities: ResolvedEntity[] }
export interface DrainResult { state: "disabled" | "deferred_profile_inactive" | "pending"; imported: number }
const withDrainLock = createMutex();
export const isCloudId = (id: string): boolean => /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(id);
export function requestOutboxDrain(): void { if (isSyncActive() && typeof window !== "undefined") window.dispatchEvent(new Event("local-outbox-ready")); }
export function drainLocalOutbox(profileId: string): Promise<DrainResult> {
  return withDrainLock(async () => {
    if (!isSyncActive()) return { state: "disabled", imported: 0 };
    const session = await getQueueSession();
    if (session.profile_id !== profileId) return { state: "deferred_profile_inactive", imported: 0 };
    await invoke("seed_local_sync_snapshot", { session });
    let after = await getImportedOutboxSequence();
    // A crash can happen after sending/removing queue entries and before ack.
    if (after > 0) await invoke("ack_local_mutations", { session, throughSequence: after });
    let imported = 0;
    while (isSyncActive()) {
      const batch = await invoke<ResolvedBatch>("list_local_mutations", { session, afterSequence: after, limit: 100 });
      if (batch.profile_id !== profileId) throw new Error("OUTBOX_PROFILE_MISMATCH");
      if (batch.through_sequence <= after) break;
      const entries: SyncQueueEntry[] = [];
      const protectedEntities: ProtectedEntity[] = [];
      const replaced = new Set<string>();
      const privateKeys = new Set<string>();
      const seedKeys = new Set<string>();
      for (const entity of batch.entities) {
        replaced.add(`${entity.kind}:${entity.id}`);
        if (entity.seed) seedKeys.add(`${entity.kind}:${entity.id}`);
        if (entity.kind === "note" && !entity.note && !entity.deleted && !entity.cloud_delete) privateKeys.add(`note:${entity.id}`);
        let operation: SyncOperation | undefined;
        let reason: ProtectedEntity["reason"] | undefined;
        if (!isCloudId(entity.id)) reason = "invalid_id";
        else if (entity.kind === "note") {
          if (entity.deleted || entity.cloud_delete) operation = { kind: "note-delete", id: entity.id };
          else if (entity.note) {
            const { meta, content } = entity.note;
            if (meta.folderId && !isCloudId(meta.folderId)) reason = "invalid_id";
            else if (!isNoteSyncable(content)) reason = "too_large";
            else if (!shouldPushNote(meta, content)) reason = "empty";
            else operation = { kind: "note-upsert", note: mapNoteToCloud(meta, content) };
          }
        } else if (entity.deleted) operation = { kind: "folder-delete", id: entity.id };
        else if (entity.folder) {
          if (!entity.folder.name.trim() || entity.folder.name.length > 200 || (entity.folder.icon?.length ?? 0) > 32) reason = "invalid_folder";
          else operation = { kind: "folder-upsert", folder: mapFolderToCloud(entity.folder) };
        }
        if (reason) protectedEntities.push({ kind: entity.kind, id: entity.id, sequence: entity.sequence, reason });
        if (operation) entries.push({ id: `outbox:${profileId}:${entity.kind}:${entity.id}:${entity.sequence}`, operation, enqueued_at: new Date().toISOString(), retry_count: 0, last_error: null, next_retry_at: null });
      }
      await importOutboxEntries(entries, batch.through_sequence, replaced, protectedEntities, privateKeys, seedKeys);
      await invoke("ack_local_mutations", { session, throughSequence: batch.through_sequence });
      imported += entries.length;
      after = batch.through_sequence;
    }
    return { state: "pending", imported };
  });
}

/** Ack means durable import, never remote confirmation. DLQ retains protection. */
export async function protectedEntityKeys(): Promise<Set<string>> {
  const snapshot = await loadSnapshot();
  const keys = new Set((snapshot.protectedEntities ?? []).map(e => `${e.kind}:${e.id}`));
  for (const entry of [...snapshot.queue, ...snapshot.deadLetters]) { const key = operationEntityKey(entry.operation); if (key) keys.add(key); }
  return keys;
}
