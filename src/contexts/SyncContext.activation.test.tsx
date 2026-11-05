// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { useContext } from "react";
import { DEFAULT_SETTINGS } from "@/lib/settings";

// ── Hoisted spies / shared state (vi.mock factories are hoisted) ─────────────
const h = vi.hoisted(() => ({
  storeData: {} as Record<string, unknown>,
  pullAllSpy: vi.fn(),
  pushSpy: vi.fn(),
  drainSpy: vi.fn(),
  noteSpy: vi.fn(),
  protectedKeys: new Set<string>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    if (cmd === "get_profile_sync_meta_path") return "meta.json";
    if (cmd === "get_or_create_device_id") return "device-B";
    if (cmd === "get_active_profile") return "default";
    if (cmd === "list_profiles") return [{ id: "default", name: "Test Profile", createdAt: "" }];
    return undefined;
  },
}));

vi.mock("@tauri-apps/plugin-store", () => ({
  Store: {
    load: async () => ({
      get: async (k: string) => h.storeData[k] ?? null,
      set: async (k: string, v: unknown) => {
        h.storeData[k] = v;
      },
      save: async () => {},
    }),
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {} }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));
vi.mock("@/lib/sync/local-outbox", () => ({ drainLocalOutbox: (...args: unknown[]) => h.drainSpy(...args), protectedEntityKeys: async () => h.protectedKeys }));

vi.mock("@/hooks/useAuth", () => ({
  useAuth: () => ({ status: "signed-in" }),
}));

vi.mock("@/hooks/useSettings", () => ({
  useSettings: () => ({
    settings: DEFAULT_SETTINGS.settings,
    updateSettings: vi.fn(async () => {}),
    subscribeToChanges: () => () => {},
  }),
}));

vi.mock("@/lib/flog", () => ({ flog: () => {} }));

vi.mock("@/lib/sync/client", () => ({
  pullAll: (...a: unknown[]) => h.pullAllSpy(...a),
  pushOperations: (...a: unknown[]) => h.pushSpy(...a),
}));

// Queue: empty / inert.
vi.mock("@/lib/sync/queue", () => ({
  enqueue: async () => {},
  enqueueWithId: async () => {},
  peekAll: async () => [],
  peekReadyHead: async () => null,
  markRetry: async () => {},
  moveToDeadLetter: async () => {},
  getDeadLetters: async () => [],
  MAX_RETRIES_BEFORE_DLQ: 5,
  size: async () => 0,
  getQueueSession: async () => ({ profile_id: "default", session_generation: 1 }),
  loadSnapshot: async () => ({ queue: [], deadLetters: [], protectedEntities: [] }),
}));

vi.mock("@/lib/sync/apply-batch-results", () => ({
  applyBatchResults: async () => ({ failedCount: 0 }),
}));

vi.mock("@/lib/sync/snippets-store", () => ({
  loadSnippets: async () => [],
  applyRemoteSnippet: async () => {},
  migrateLegacySnippetsOnce: async () => {},
}));

vi.mock("@/lib/sync/notes-store", () => ({
  applyRemoteNote: (...args: unknown[]) => h.noteSpy(...args),
  flushPendingNoteUpdates: async () => {},
  listNotes: async () => [],
  readNote: async () => ({ meta: {}, content: "" }),
  scanOversizedNoteCount: async () => 0,
}));

vi.mock("@/lib/sync/folders-store", () => ({
  applyRemoteFolder: async () => {},
  listFolders: async () => [],
}));

vi.mock("@/lib/sync/dictionary-store", () => ({
  loadDictionary: async () => ({ words: [] }),
  applyRemoteWord: async () => {},
  migrateLegacyDictionaryOnce: async () => {},
}));

import { SyncContext, SyncProvider } from "./SyncContext";

const EMPTY_PULL = {
  settings: null,
  dictionary: [],
  snippets: [],
  notes: [],
  folders: [],
  profiles: [],
  invalid: { settings: false, dictionary: 0, snippets: 0, notes: 0, folders: 0, profiles: 0 },
};

beforeEach(() => {
  Object.keys(h.storeData).forEach((k) => delete h.storeData[k]);
  h.pullAllSpy.mockReset().mockResolvedValue(EMPTY_PULL);
  h.pushSpy.mockReset().mockImplementation(async (ops: unknown[]) => ({ ok: true, results: ops.map((_, index) => ({ index, ok: true })), server_time: "2026-06-25T00:00:00Z" }));
  h.drainSpy.mockReset().mockResolvedValue({ state: "pending", imported: 0 });
  h.noteSpy.mockReset().mockResolvedValue({ applied: true, deferred: false });
  h.protectedKeys.clear();
});

describe("SyncContext — activation forces a full reconcile", () => {
  it("enableSync pulls with since=null even when a stale cursor is stored", async () => {
    // Simulate a device that synced before: a residual incremental cursor.
    h.storeData["enabled"] = false;
    h.storeData["last_pull_at"] = "2020-01-01T00:00:00Z";

    const { result } = renderHook(() => useContext(SyncContext), {
      wrapper: SyncProvider,
    });

    await act(async () => {
      await result.current!.enableSync();
    });

    // Activation MUST pull with since=null (full reconcile). A stale cursor
    // would otherwise make it incremental and miss the older notes.
    expect(h.pullAllSpy).toHaveBeenCalled();
    const sinceArgs = h.pullAllSpy.mock.calls.map((c) => c[0]);
    expect(sinceArgs).toContain(null);
    expect(sinceArgs).not.toContain("2020-01-01T00:00:00Z");
  });

  it("disableSync clears the stored pull cursor", async () => {
    // Mount disabled (stable, no activation pull) — disableSync clears the
    // cursor unconditionally so a later re-enable always full-reconciles.
    h.storeData["enabled"] = false;
    h.storeData["last_pull_at"] = "2026-06-01T00:00:00Z";

    const { result } = renderHook(() => useContext(SyncContext), {
      wrapper: SyncProvider,
    });

    await act(async () => {
      await result.current!.disableSync();
    });

    expect(h.storeData["last_pull_at"]).toBeNull();
  });
  it("absorbs work intentions before pull and confirms profile in a separate request", async () => {
    const { result } = renderHook(() => useContext(SyncContext), { wrapper: SyncProvider });
    await act(async () => { await result.current!.enableSync(); });
    expect(h.drainSpy.mock.invocationCallOrder[0]).toBeLessThan(h.pullAllSpy.mock.invocationCallOrder[0]);
    const firstOps = h.pushSpy.mock.calls[0][0];
    expect(firstOps).toHaveLength(1);
    expect(firstOps[0].kind).toBe("profile-upsert");
  });
  it("deferred rows retain a full reread after queue confirmation or DLQ discard", async () => {
    h.protectedKeys.add("note:n");
    h.pullAllSpy.mockResolvedValue({ ...EMPTY_PULL, notes: [{ id: "n", title: "remote", content_html: "remote", folder_id: null, favorite: false, order: 0, created_at: "2026-11-05T00:00:00Z", updated_at: "2026-11-05T00:00:00Z", deleted_at: null }] });
    const { result } = renderHook(() => useContext(SyncContext), { wrapper: SyncProvider });
    await act(async () => { await result.current!.enableSync(); });
    expect(h.noteSpy).not.toHaveBeenCalled();
    expect(h.storeData.last_pull_at).toBeNull();
    h.protectedKeys.clear();
    await act(async () => { await result.current!.syncNow(); });
    expect(h.pullAllSpy.mock.calls[h.pullAllSpy.mock.calls.length - 1][0]).toBeNull();
    expect(h.noteSpy).toHaveBeenCalledWith(expect.objectContaining({ id: "n" }));
  });
  it("serializes overlapping pulls so an older response cannot follow a newer tombstone purge", async () => {
    let release: (() => void) | undefined;
    let active = 0;
    let maximum = 0;
    h.pullAllSpy.mockImplementation(async () => {
      active++; maximum = Math.max(maximum, active);
      if (!release) await new Promise<void>(resolve => { release = resolve; });
      active--; return EMPTY_PULL;
    });
    const { result } = renderHook(() => useContext(SyncContext), { wrapper: SyncProvider });
    let activation: Promise<void> | undefined;
    await act(async () => { activation = result.current!.enableSync(); await vi.waitFor(() => expect(release).toBeTypeOf("function")); });
    await act(async () => { await result.current!.syncNow(); });
    expect(h.pullAllSpy).toHaveBeenCalledOnce();
    await act(async () => { release!(); await activation; });
    expect(h.pullAllSpy.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(maximum).toBe(1);
  });
});
