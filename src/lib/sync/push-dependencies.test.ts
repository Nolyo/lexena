import { describe, expect, it } from "vitest";
import { nextDependencyBatch } from "./push-dependencies";
import type { SyncOperation, SyncQueueEntry } from "./types";
const entry = (id: string, operation: SyncOperation, next_retry_at: string | null = null): SyncQueueEntry => ({ id, operation, next_retry_at, retry_count: 0, last_error: null, enqueued_at: "now" });
const folder = entry("folder", { kind: "folder-upsert", folder: { id: "f", name: "Folder", icon: null, order: 0, updated_at: "now", deleted_at: null } });
const note = entry("note", { kind: "note-upsert", note: { id: "n", title: "Note", content_html: "text", folder_id: "f", favorite: false, order: 0, updated_at: "now", deleted_at: null } });
const profile = entry("profile", { kind: "profile-upsert", profile: { id: "p", name: "Profile", updated_at: "now", deleted_at: null } });
describe("sync dependency phases", () => {
  it("profile_parent_confirmed_before_folder_and_note", () => {
    const queue = [note, folder, profile];
    expect(nextDependencyBatch(queue, []).map(e => e.id)).toEqual(["profile"]);
    expect(nextDependencyBatch(queue.filter(e => e.id !== "profile"), []).map(e => e.id)).toEqual(["folder"]);
    expect(nextDependencyBatch([note], []).map(e => e.id)).toEqual(["note"]);
  });
  it("failed parent remains a barrier during backoff and DLQ", () => {
    expect(nextDependencyBatch([note, { ...folder, next_retry_at: "2099-01-01" }], [])).toEqual([]);
    expect(nextDependencyBatch([note], [folder])).toEqual([]);
    expect(nextDependencyBatch([note, folder], [profile])).toEqual([]);
  });
  it("a note detach/delete is confirmed before deleting its former folder", () => {
    const deletion = entry("delete-folder", { kind: "folder-delete", id: "f" });
    expect(nextDependencyBatch([deletion, note], []).map(e => e.id)).toEqual(["note"]);
  });
  it("unpushable_folder_blocks_child_until_corrected_and_confirmed", () => {
    expect(nextDependencyBatch([note], [], Date.now(), new Set(["f"]))).toEqual([]);
    expect(nextDependencyBatch([note, folder], []).map(e => e.id)).toEqual(["folder"]);
    expect(nextDependencyBatch([note], []).map(e => e.id)).toEqual(["note"]);
  });
});
