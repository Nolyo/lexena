import { beforeEach, describe, expect, it, vi } from "vitest";
const h = vi.hoisted(() => ({ invoke: vi.fn(), wake: vi.fn(), enqueue: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));
vi.mock("./local-outbox", () => ({ requestOutboxDrain: h.wake }));
vi.mock("./queue", () => ({ getQueueSession: async () => ({ profile_id: "work", session_generation: 7 }), enqueue: h.enqueue }));
import { applyRemoteFolder, createFolderSynced, deleteFolderSynced, renameFolderSynced, pushFolderUpsert } from "./folders-store";
import type { CloudUserFolderRow, LocalFolderMeta } from "./types";
beforeEach(() => { h.invoke.mockReset().mockResolvedValue({ id: "folder" }); h.wake.mockClear(); h.enqueue.mockClear(); });
describe("folder mutations use native intent", () => {
  it.each([
    ["create_folder", () => createFolderSynced("Name", "x"), { name: "Name", icon: "x" }],
    ["rename_folder", () => renameFolderSynced("folder", "New"), { id: "folder", name: "New", icon: null }],
    ["delete_folder", () => deleteFolderSynced("folder"), { id: "folder" }],
  ] as const)("%s commits once without duplicate enqueue", async (command, action, args) => { await action(); expect(h.invoke).toHaveBeenCalledExactlyOnceWith(command, { ...args, session: { profile_id: "work", session_generation: 7 } }); expect(h.wake).toHaveBeenCalledOnce(); expect(h.enqueue).not.toHaveBeenCalled(); });
  it("a reorder notification does not ship a captured stale folder", async () => { await pushFolderUpsert({ id: "old" } as LocalFolderMeta); expect(h.invoke).not.toHaveBeenCalled(); expect(h.wake).toHaveBeenCalledOnce(); expect(h.enqueue).not.toHaveBeenCalled(); });
  it("remote folder imports are session bound and do not reenqueue", async () => {
    const row = { id: "f", name: "remote", icon: "x", created_at: "2026-11-05", updated_at: "2026-11-05", order: 0, deleted_at: null } as CloudUserFolderRow;
    h.invoke.mockResolvedValue({ applied: true, deferred: false }); expect(await applyRemoteFolder(row)).toEqual({ applied: true, deferred: false });
    expect(h.invoke).toHaveBeenCalledWith("apply_sync_remote_folder", { session: { profile_id: "work", session_generation: 7 }, folder: { id: "f", name: "remote", icon: "x", createdAt: "2026-11-05", updatedAt: "2026-11-05", order: 0 } }); expect(h.wake).not.toHaveBeenCalled();
  });
});
