import type { SyncQueueEntry } from "./types";
function rank(entry: SyncQueueEntry): number {
  if (entry.operation.kind === "profile-upsert" || entry.operation.kind === "profile-delete") return 0;
  if (entry.operation.kind === "folder-upsert") return 1;
  if (entry.operation.kind === "note-upsert" || entry.operation.kind === "note-delete") return 3;
  if (entry.operation.kind === "folder-delete") return 4;
  return 2;
}
/** A dependency phase is confirmed before the next phase is ever sent. */
export function nextDependencyBatch(queue: SyncQueueEntry[], deadLetters: SyncQueueEntry[], now = Date.now(), protectedFolderIds: ReadonlySet<string> = new Set()): SyncQueueEntry[] {
  if (!queue.length) return [];
  const phase = Math.min(...queue.map(rank));
  const blockedFolders = new Set(deadLetters.flatMap(e => e.operation.kind === "folder-upsert" ? [e.operation.folder.id] : []));
  for (const id of protectedFolderIds) blockedFolders.add(id);
  if (deadLetters.some(e => e.operation.kind === "profile-upsert" || e.operation.kind === "profile-delete")) return [];
  return queue.filter(e => rank(e) === phase && (!e.next_retry_at || new Date(e.next_retry_at).getTime() <= now) && !(e.operation.kind === "note-upsert" && e.operation.note.folder_id && blockedFolders.has(e.operation.note.folder_id))).slice(0, phase === 0 ? 1 : 50);
}
