import { describe, it, expect, beforeEach, vi } from "vitest";

const storeData: Record<string, unknown> = {};
vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "get_profile_session") {
      return { profile_id: "default", session_generation: 1 };
    }
    if (cmd === "load_sync_queue") {
      return structuredClone(storeData.snapshot ?? {
        queue: [], deadLetters: [], importedOutboxSequence: 0, revision: "absent-v1",
      });
    }
    if (cmd === "save_sync_queue") {
      const current = storeData.snapshot as { revision: string } | undefined;
      if (args?.expectedRevision !== (current?.revision ?? "absent-v1")) {
        throw new Error("SYNC_QUEUE_REVISION_CONFLICT");
      }
      storeData.snapshot = { ...structuredClone(args?.snapshot as object), revision: crypto.randomUUID() };
      return structuredClone(storeData.snapshot);
    }
    throw new Error(`unexpected invoke ${cmd}`);
  },
}));
vi.mock("@tauri-apps/plugin-store", () => {
  return {
    Store: {
      load: async () => ({
        get: async (k: string) => storeData[k] ?? null,
        set: async (k: string, v: unknown) => {
          storeData[k] = v;
        },
        save: async () => {},
      }),
    },
  };
});

import {
  enqueue,
  peekAll,
  __resetForTests as resetQueue,
} from "./queue";
import { applyBatchResults } from "./apply-batch-results";

describe("applyBatchResults — partial success", () => {
  beforeEach(() => {
    Object.keys(storeData).forEach((k) => delete storeData[k]);
    resetQueue();
  });

  it("garde l'op #1 (fail), retire #0 et #2 (ok)", async () => {
    const e0 = await enqueue({ kind: "dictionary-upsert", word: "a" });
    const e1 = await enqueue({ kind: "dictionary-upsert", word: "b" });
    const e2 = await enqueue({ kind: "dictionary-upsert", word: "c" });

    const batch = [e0, e1, e2];
    const results = [
      { index: 0, ok: true },
      { index: 1, ok: false, error: "constraint" },
      { index: 2, ok: true },
    ];

    const { failedCount } = await applyBatchResults(batch, results);

    expect(failedCount).toBe(1);

    const remaining = await peekAll();
    expect(remaining.map((e) => e.operation.kind)).toEqual(["dictionary-upsert"]);
    expect((remaining[0].operation as { kind: string; word: string }).word).toBe("b");
    expect(remaining[0].retry_count).toBe(1);
    expect(remaining[0].last_error).toBe("constraint");
  });

  it("retire toutes les ops si toutes ok", async () => {
    const e0 = await enqueue({ kind: "dictionary-upsert", word: "a" });
    const e1 = await enqueue({ kind: "dictionary-upsert", word: "b" });

    const { failedCount } = await applyBatchResults(
      [e0, e1],
      [
        { index: 0, ok: true },
        { index: 1, ok: true },
      ]
    );

    expect(failedCount).toBe(0);
    expect(await peekAll()).toEqual([]);
  });
});
