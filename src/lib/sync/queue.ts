import { invoke } from "@tauri-apps/api/core";
import type { SyncOperation, SyncQueueEntry } from "./types";
import { createMutex } from "./_mutex";

export const MAX_RETRIES_BEFORE_DLQ = 5;
export interface ProfileQueueSession { profile_id: string; session_generation: number }
export interface QueueSnapshot {
  queue: SyncQueueEntry[];
  deadLetters: SyncQueueEntry[];
  importedOutboxSequence: number;
  revision: string;
  protectedEntities?: ProtectedEntity[];
}
export interface ProtectedEntity { kind: "note" | "folder"; id: string; sequence: number; reason: "empty" | "too_large" | "invalid_id" | "invalid_folder" }
let sessionPromise: Promise<ProfileQueueSession> | null = null;
const withLock = createMutex();

/** Captured once per renderer; a late callback must never adopt another profile. */
export function getQueueSession(): Promise<ProfileQueueSession> {
  return sessionPromise ??= invoke<ProfileQueueSession>("get_profile_session");
}
export function __resetForTests() { sessionPromise = null; }

export async function loadSnapshot(): Promise<QueueSnapshot> {
  return invoke<QueueSnapshot>("load_sync_queue", { session: await getQueueSession() });
}
async function changeSnapshot<T>(change: (snapshot: QueueSnapshot) => T): Promise<T> {
  return withLock(async () => {
    for (let attempt = 0; attempt < 3; attempt++) {
      const snapshot = await loadSnapshot();
      const result = change(snapshot);
      try {
        await invoke<QueueSnapshot>("save_sync_queue", {
          session: await getQueueSession(), snapshot, expectedRevision: snapshot.revision,
        });
        return result;
      } catch (error) {
        if (!String(error).includes("SYNC_QUEUE_REVISION_CONFLICT") || attempt === 2) throw error;
      }
    }
    throw new Error("SYNC_QUEUE_REVISION_CONFLICT");
  });
}

function makeEntry(operation: SyncOperation, id: string = crypto.randomUUID()): SyncQueueEntry {
  return { id, operation, enqueued_at: new Date().toISOString(), retry_count: 0, last_error: null, next_retry_at: null };
}
export async function enqueue(op: SyncOperation): Promise<SyncQueueEntry> {
  const entry = makeEntry(op);
  return changeSnapshot((snapshot) => { snapshot.queue.push(entry); return entry; });
}
export async function enqueueWithId(id: string, op: SyncOperation): Promise<SyncQueueEntry> {
  const entry = makeEntry(op, id);
  return changeSnapshot((snapshot) => {
    const existing = [...snapshot.queue, ...snapshot.deadLetters].find((item) => item.id === id);
    if (existing) return existing;
    snapshot.queue.push(entry);
    return entry;
  });
}
export async function peekAll(): Promise<SyncQueueEntry[]> { return (await loadSnapshot()).queue; }
export async function peekHead(): Promise<SyncQueueEntry | null> { return (await peekAll())[0] ?? null; }
export async function peekReadyHead(): Promise<SyncQueueEntry | null> {
  const head = await peekHead();
  return head && (!head.next_retry_at || new Date(head.next_retry_at).getTime() <= Date.now()) ? head : null;
}
export async function dequeue(): Promise<SyncQueueEntry | null> {
  return changeSnapshot((snapshot) => snapshot.queue.shift() ?? null);
}
export async function dequeueById(id: string): Promise<SyncQueueEntry | null> {
  return changeSnapshot((snapshot) => {
    const index = snapshot.queue.findIndex((entry) => entry.id === id);
    return index < 0 ? null : snapshot.queue.splice(index, 1)[0];
  });
}
export async function markRetry(id: string, error: string): Promise<void> {
  await changeSnapshot((snapshot) => {
    const entry = snapshot.queue.find((item) => item.id === id);
    if (!entry) return;
    entry.retry_count++;
    entry.last_error = error;
    entry.next_retry_at = new Date(Date.now() + backoffMs(entry.retry_count - 1)).toISOString();
  });
}
export async function size(): Promise<number> { return (await peekAll()).length; }
export async function clear(): Promise<void> { await changeSnapshot((snapshot) => { snapshot.queue = []; }); }
export function backoffMs(retryCount: number): number {
  const delays = [1_000, 5_000, 30_000, 120_000, 300_000];
  return delays[Math.min(retryCount, delays.length - 1)];
}
export async function moveToDeadLetter(id: string, error: string): Promise<void> {
  await changeSnapshot((snapshot) => {
    const index = snapshot.queue.findIndex((entry) => entry.id === id);
    if (index < 0) return;
    const [entry] = snapshot.queue.splice(index, 1);
    snapshot.deadLetters.push({ ...entry, last_error: error });
  });
}
export async function getDeadLetters(): Promise<SyncQueueEntry[]> { return (await loadSnapshot()).deadLetters; }
export async function removeDeadLetter(id: string): Promise<void> {
  await changeSnapshot((snapshot) => { snapshot.deadLetters = snapshot.deadLetters.filter((entry) => entry.id !== id); });
}
/** Retry only a still-persisted intention; a stale dialog cannot recreate it. */
export async function retryDeadLetter(id: string): Promise<SyncQueueEntry | null> {
  return changeSnapshot((snapshot) => {
    const index = snapshot.deadLetters.findIndex((entry) => entry.id === id);
    if (index < 0) return null;
    const [entry] = snapshot.deadLetters.splice(index, 1);
    const retried = { ...entry, retry_count: 0, last_error: null, next_retry_at: null };
    snapshot.queue.push(retried);
    return retried;
  });
}
export async function getImportedOutboxSequence(): Promise<number> { return (await loadSnapshot()).importedOutboxSequence; }

/** Save entries and checkpoint atomically; checkpoint survives their consumption. */
export function operationEntityKey(op: SyncOperation): string | null {
  switch (op.kind) {
    case "note-upsert": return `note:${op.note.id}`;
    case "note-delete": return `note:${op.id}`;
    case "folder-upsert": return `folder:${op.folder.id}`;
    case "folder-delete": return `folder:${op.id}`;
    default: return null;
  }
}
export async function importOutboxEntries(entries: SyncQueueEntry[], throughSequence: number, replacedKeys: ReadonlySet<string> = new Set(), protectedEntities: ProtectedEntity[] = [], privateKeys: ReadonlySet<string> = new Set(), seedKeys: ReadonlySet<string> = new Set()): Promise<void> {
  if (!Number.isSafeInteger(throughSequence) || throughSequence < 0) throw new Error("INVALID_OUTBOX_SEQUENCE");
  await changeSnapshot((snapshot) => {
    if (throughSequence <= snapshot.importedOutboxSequence) return;
    const seededDeadLetters: SyncQueueEntry[] = [];
    const importedEntries = entries.map(entry => {
      const key = operationEntityKey(entry.operation) ?? "";
      if (!seedKeys.has(key)) return entry;
      const failed = snapshot.deadLetters.find(e => operationEntityKey(e.operation) === key);
      const old = failed ?? snapshot.queue.find(e => operationEntityKey(e.operation) === key);
      const refreshed = old ? { ...entry, retry_count: old.retry_count, next_retry_at: old.next_retry_at, last_error: old.last_error } : entry;
      if (failed) seededDeadLetters.push(refreshed);
      return refreshed;
    }).filter(entry => !seededDeadLetters.includes(entry));
    const keep = (entry: SyncQueueEntry) => {
      const key = operationEntityKey(entry.operation) ?? "";
      return !replacedKeys.has(key) || (privateKeys.has(key) && entry.operation.kind === "note-delete" && !entries.some(e => operationEntityKey(e.operation) === key));
    };
    snapshot.queue = snapshot.queue.filter(keep);
    snapshot.deadLetters = snapshot.deadLetters.filter(keep);
    snapshot.deadLetters.push(...seededDeadLetters);
    snapshot.protectedEntities = [...(snapshot.protectedEntities ?? []).filter((entity) => !replacedKeys.has(`${entity.kind}:${entity.id}`)), ...protectedEntities];
    const known = new Set([...snapshot.queue, ...snapshot.deadLetters].map((entry) => entry.id));
    for (const entry of importedEntries) {
      if (!known.has(entry.id)) { snapshot.queue.push(entry); known.add(entry.id); }
    }
    snapshot.importedOutboxSequence = throughSequence;
  });
}
