import { beforeEach, describe, expect, it, vi } from "vitest";
const h = vi.hoisted(() => ({ invoke: vi.fn(), wake: vi.fn(), enqueue: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));
vi.mock("./local-outbox", () => ({ requestOutboxDrain: h.wake }));
vi.mock("./queue", () => ({ getQueueSession: async () => ({ profile_id: "work", session_generation: 7 }), enqueue: h.enqueue }));
import { applyRemoteNote, createNoteSynced, deleteNoteSynced, listNotes, moveNoteToFolderSynced, readNote, scanOversizedNoteCount, setNoteLocalOnlySynced, toggleNoteFavoriteSynced, updateNoteSynced } from "./notes-store";
import type { CloudUserNoteRow } from "./types";
beforeEach(() => { h.invoke.mockReset().mockResolvedValue({ id: "note" }); h.wake.mockClear(); h.enqueue.mockClear(); });
describe("notes store uses the native outbox authority", () => {
  it.each([
    ["create_note", () => createNoteSynced(null), { folderId: null }],
    ["update_note", () => updateNoteSynced("note", "latest", "Title"), { id: "note", content: "latest", title: "Title" }],
    ["delete_note", () => deleteNoteSynced("note"), { id: "note" }],
    ["toggle_note_favorite", () => toggleNoteFavoriteSynced("note"), { id: "note" }],
    ["move_note_to_folder", () => moveNoteToFolderSynced("note", null), { noteId: "note", folderId: null }],
    ["set_note_local_only", () => setNoteLocalOnlySynced("note", true), { id: "note", localOnly: true }],
  ] as const)("%s commits once and only wakes the main drain", async (command, action, args) => {
    await action(); expect(h.invoke).toHaveBeenCalledExactlyOnceWith(command, { ...args, session: { profile_id: "work", session_generation: 7 } }); expect(h.wake).toHaveBeenCalledOnce(); expect(h.enqueue).not.toHaveBeenCalled();
  });
  it("failed local mutation never wakes the consumer", async () => { h.invoke.mockRejectedValue(Error("disk")); await expect(updateNoteSynced("n", "x", "T")).rejects.toThrow("disk"); expect(h.wake).not.toHaveBeenCalled(); });
  it("read helpers do not produce intentions", async () => { await listNotes(); await readNote("n"); expect(h.wake).not.toHaveBeenCalled(); });
  it("counts UTF8 oversize public notes while skipping private/deleted", async () => {
    h.invoke.mockImplementation(async (command, args) => command === "list_notes" ? [{ id: "large" }, { id: "small" }, { id: "private", localOnly: true }, { id: "deleted", deletedAt: "x" }] : { content: args.id === "large" ? "é".repeat(1_572_865) : "small" });
    expect(await scanOversizedNoteCount()).toBe(1); expect(h.invoke).toHaveBeenCalledTimes(3);
  });
  it("remote_import_does_not_reenqueue and captures the original profile session", async () => {
    const row = { id: "n", title: "remote", content_html: "html", created_at: "2026-11-05", updated_at: "2026-11-05", folder_id: null, favorite: false, order: 0, deleted_at: null } as CloudUserNoteRow;
    h.invoke.mockResolvedValue({ applied: false, deferred: true });
    expect(await applyRemoteNote(row)).toEqual({ applied: false, deferred: true });
    expect(h.invoke).toHaveBeenCalledWith("apply_sync_remote_note", expect.objectContaining({ session: { profile_id: "work", session_generation: 7 }, content: "html", meta: expect.objectContaining({ id: "n", title: "remote" }) }));
    expect(h.wake).not.toHaveBeenCalled(); expect(h.enqueue).not.toHaveBeenCalled();
  });
});
