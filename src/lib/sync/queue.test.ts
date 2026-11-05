import { describe, it, expect, beforeEach, vi } from "vitest";
import { retryDeadLetter } from "./queue";

const storeData: Record<string, unknown> = {};
let activeSession = { profile_id: "default", session_generation: 1 };
let sequence = 0;
let failNextSave = false;
const invokeMock = vi.fn(async (cmd: string, args?: unknown) => {
  if (cmd === "get_profile_session") return { ...activeSession };
  const input = args as { session: typeof activeSession; snapshot: QueueSnapshot; expectedRevision: string };
  if (input?.session.profile_id !== activeSession.profile_id || input?.session.session_generation !== activeSession.session_generation) throw new Error("EDITOR_SESSION_EXPIRED");
  const current = storeData[activeSession.profile_id] as QueueSnapshot | undefined ?? { queue: [], deadLetters: [], importedOutboxSequence: 0, revision: "absent-v1" };
  if (cmd === "load_sync_queue") return structuredClone(current);
  if (cmd === "save_sync_queue") {
    if (failNextSave) { failNextSave = false; throw new Error("SYNC_QUEUE_SAVE_FAILED"); }
    if (current.revision !== input.expectedRevision) throw new Error("SYNC_QUEUE_REVISION_CONFLICT");
    const saved = { ...structuredClone(input.snapshot), revision: `revision-${++sequence}` };
    storeData[activeSession.profile_id] = saved;
    return structuredClone(saved);
  }
  throw new Error(`unexpected invoke ${cmd}`);
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args) }));

import {
  enqueue,
  peekAll,
  dequeue,
  dequeueById,
  markRetry,
  size,
  clear,
  peekReadyHead,
  moveToDeadLetter,
  getDeadLetters,
  removeDeadLetter,
  MAX_RETRIES_BEFORE_DLQ,
  __resetForTests,
  importOutboxEntries,
  getImportedOutboxSequence,
  type QueueSnapshot,
} from "./queue";
import type { SyncOperation } from "./types";

const OP: SyncOperation = { kind: "dictionary-upsert", word: "hello" };

beforeEach(() => {
  activeSession = { profile_id: "default", session_generation: 1 };
  sequence = 0;
  failNextSave = false;
});

describe("sync queue", () => {
  beforeEach(async () => {
    Object.keys(storeData).forEach((k) => delete storeData[k]);
    invokeMock.mockClear();
    __resetForTests();
  });

  it("rejects a late write after the captured profile session changes", async () => {
    await enqueue(OP);
    activeSession = { profile_id: "work", session_generation: 2 };
    await expect(enqueue(OP)).rejects.toThrow("EDITOR_SESSION_EXPIRED");
    __resetForTests();
    expect(await size()).toBe(0);
    activeSession = { profile_id: "default", session_generation: 3 };
    __resetForTests();
    expect(await size()).toBe(1);
  });

  it("starts empty", async () => {
    expect(await size()).toBe(0);
  });

  it("retry moves the currently persisted DLQ item atomically and resets backoff", async () => {
    const failed = await enqueue(OP);
    await markRetry(failed.id, "offline");
    await moveToDeadLetter(failed.id, "offline");
    const existing = await enqueue({ kind: "dictionary-upsert", word: "keep" });
    const persisted = (await getDeadLetters())[0];
    invokeMock.mockClear();
    const retried = await retryDeadLetter(persisted.id);
    expect(retried).toEqual({ ...persisted, retry_count: 0, last_error: null, next_retry_at: null });
    expect(await getDeadLetters()).toEqual([]);
    expect((await peekAll()).map(entry => entry.id)).toEqual([existing.id, persisted.id]);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "save_sync_queue")).toHaveLength(1);
    expect(activeSession.profile_id).toBe("default");
  });

  it("a stale dialog retry cannot recreate a consumed superseding delete", async () => {
    const id = "11111111-1111-4111-8111-111111111111";
    const stale = await enqueue({ kind: "note-upsert", note: { id, title: "old", content_html: "old captured text", folder_id: null, favorite: false, order: 0, updated_at: "2026-11-05T00:00:00Z", deleted_at: null } });
    await moveToDeadLetter(stale.id, "offline");
    const dialogItem = (await getDeadLetters())[0];
    const deletion = { ...stale, id: "outbox:default:note:2", operation: { kind: "note-delete" as const, id } };
    await importOutboxEntries([deletion], 2, new Set([`note:${id}`]));
    await dequeueById(deletion.id);
    expect(await retryDeadLetter(dialogItem.id)).toBeNull();
    expect(await peekAll()).toEqual([]);
    expect(await getDeadLetters()).toEqual([]);
    expect(await getImportedOutboxSequence()).toBe(2);
  });

  it("retry save failure leaves the persisted DLQ and queue intact", async () => {
    const failed = await enqueue(OP); await moveToDeadLetter(failed.id, "offline");
    failNextSave = true;
    await expect(retryDeadLetter(failed.id)).rejects.toThrow("SYNC_QUEUE_SAVE_FAILED");
    expect(await peekAll()).toEqual([]);
    expect((await getDeadLetters()).map(entry => entry.id)).toEqual([failed.id]);
  });

  it("retry rechecks DLQ membership after a concurrent supersession CAS conflict", async () => {
    const failed = await enqueue(OP); await moveToDeadLetter(failed.id, "offline");
    const originalInvoke = invokeMock.getMockImplementation()!;
    let superseded = false;
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "save_sync_queue" && !superseded) {
        superseded = true;
        storeData[activeSession.profile_id] = { queue: [], deadLetters: [], importedOutboxSequence: 2, revision: "newer-supersession" };
        throw new Error("SYNC_QUEUE_REVISION_CONFLICT");
      }
      return originalInvoke(cmd, args);
    });
    try {
      expect(await retryDeadLetter(failed.id)).toBeNull();
      expect(await peekAll()).toEqual([]); expect(await getDeadLetters()).toEqual([]);
      expect(await getImportedOutboxSequence()).toBe(2);
    } finally { invokeMock.mockImplementation(originalInvoke); }
  });

  it("enqueue adds entries FIFO", async () => {
    await enqueue(OP);
    await enqueue({ kind: "dictionary-delete", word: "bye" });
    const all = await peekAll();
    expect(all).toHaveLength(2);
    expect(all[0].operation.kind).toBe("dictionary-upsert");
    expect(all[1].operation.kind).toBe("dictionary-delete");
  });

  it("dequeue removes head FIFO", async () => {
    await enqueue(OP);
    await enqueue({ kind: "dictionary-delete", word: "bye" });
    const first = await dequeue();
    expect(first?.operation.kind).toBe("dictionary-upsert");
    expect(await size()).toBe(1);
  });

  it("markRetry increments retry_count and stores error", async () => {
    await enqueue(OP);
    const all = await peekAll();
    await markRetry(all[0].id, "network timeout");
    const after = await peekAll();
    expect(after[0].retry_count).toBe(1);
    expect(after[0].last_error).toBe("network timeout");
  });

  it("clear empties queue", async () => {
    await enqueue(OP);
    await clear();
    expect(await size()).toBe(0);
  });

  it("does not reimport consumed outbox operations after restart before acknowledgement", async () => {
    const entry = { id: "outbox-7", operation: OP, enqueued_at: "2026-11-05T10:00:00Z", retry_count: 0, last_error: null, next_retry_at: null };
    await importOutboxEntries([entry], 7);
    expect(await size()).toBe(1);
    await dequeue();
    __resetForTests();
    await importOutboxEntries([entry], 7);
    expect(await size()).toBe(0);
    expect(await getImportedOutboxSequence()).toBe(7);
  });

  it("persists queue removal and dead letter addition together", async () => {
    const entry = await enqueue(OP);
    failNextSave = true;
    await expect(moveToDeadLetter(entry.id, "failed")).rejects.toThrow("SYNC_QUEUE_SAVE_FAILED");
    expect((await peekAll()).map((item) => item.id)).toEqual([entry.id]);
    expect(await getDeadLetters()).toEqual([]);
  });
});

describe("dequeueById", () => {
  beforeEach(async () => {
    Object.keys(storeData).forEach((k) => delete storeData[k]);
    __resetForTests();
  });

  it("retire uniquement l'entrée matchant l'id, préserve l'ordre des autres", async () => {
    const e1 = await enqueue({ kind: "dictionary-upsert", word: "alpha" });
    const e2 = await enqueue({ kind: "dictionary-upsert", word: "beta" });
    const e3 = await enqueue({ kind: "dictionary-upsert", word: "gamma" });

    const removed = await dequeueById(e2.id);
    expect(removed?.id).toBe(e2.id);

    const remaining = await peekAll();
    expect(remaining.map((e) => e.id)).toEqual([e1.id, e3.id]);
  });

  it("retourne null si l'id n'existe plus", async () => {
    const removed = await dequeueById("non-existent");
    expect(removed).toBeNull();
  });
});

describe("backoff respect", () => {
  beforeEach(async () => {
    Object.keys(storeData).forEach((k) => delete storeData[k]);
    __resetForTests();
  });

  it("peekReadyHead retourne null si la head a un next_retry_at futur", async () => {
    const e = await enqueue({ kind: "dictionary-upsert", word: "x" });
    await markRetry(e.id, "boom"); // next_retry_at = now + 1s
    const ready = await peekReadyHead();
    expect(ready).toBeNull();
  });

  it("peekReadyHead retourne la head si next_retry_at est passé", async () => {
    const e = await enqueue({ kind: "dictionary-upsert", word: "x" });
    await markRetry(e.id, "boom");
    vi.useFakeTimers();
    vi.setSystemTime(new Date(Date.now() + 60_000));
    const ready = await peekReadyHead();
    expect(ready?.id).toBe(e.id);
    vi.useRealTimers();
  });

  it("peekReadyHead retourne la head si next_retry_at est null (jamais retry)", async () => {
    const e = await enqueue({ kind: "dictionary-upsert", word: "x" });
    const ready = await peekReadyHead();
    expect(ready?.id).toBe(e.id);
  });
});

describe("dead letter queue", () => {
  beforeEach(async () => {
    Object.keys(storeData).forEach((k) => delete storeData[k]);
    __resetForTests();
  });

  it("moveToDeadLetter retire de la queue et ajoute au DLQ", async () => {
    const e = await enqueue({ kind: "snippet-delete", id: "snip-1" });
    await moveToDeadLetter(e.id, "permanent failure");
    expect((await peekAll()).length).toBe(0);
    const dlq = await getDeadLetters();
    expect(dlq.length).toBe(1);
    expect(dlq[0].id).toBe(e.id);
    expect(dlq[0].last_error).toBe("permanent failure");
  });

  it("removeDeadLetter retire l'entrée du DLQ", async () => {
    const e = await enqueue({ kind: "snippet-delete", id: "snip-1" });
    await moveToDeadLetter(e.id, "boom");
    expect((await getDeadLetters()).length).toBe(1);
    await removeDeadLetter(e.id);
    expect((await getDeadLetters()).length).toBe(0);
  });

  it("MAX_RETRIES_BEFORE_DLQ est exporté", () => {
    expect(typeof MAX_RETRIES_BEFORE_DLQ).toBe("number");
    expect(MAX_RETRIES_BEFORE_DLQ).toBeGreaterThan(0);
  });
});

describe("markRetry sets next_retry_at", () => {
  beforeEach(async () => {
    Object.keys(storeData).forEach((k) => delete storeData[k]);
    __resetForTests();
  });

  it("populate next_retry_at avec le backoff après markRetry", async () => {
    const e = await enqueue({ kind: "dictionary-upsert", word: "x" });
    const before = Date.now();
    await markRetry(e.id, "boom");
    const all = await peekAll();
    expect(all[0].next_retry_at).not.toBeNull();
    const next = new Date(all[0].next_retry_at as string).getTime();
    expect(next).toBeGreaterThanOrEqual(before);
  });
});
