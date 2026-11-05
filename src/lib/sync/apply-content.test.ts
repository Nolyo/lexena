import { describe, expect, it } from "vitest";
import { applyContentInDependencyOrder, normalizePulledNoteFolders } from "./apply-content";

describe("applyContentInDependencyOrder", () => {
  it("recovers a fresh-device active note whose cloud parent has been soft-deleted", async () => {
    const folders = [{ id: "deleted-folder", deleted_at: "2026-11-05T10:00:00Z" }];
    const pulledNotes = [{ id: "note", folder_id: "deleted-folder", deleted_at: null, content: "Keep this note" }];
    const restored: string[] = [];
    await applyContentInDependencyOrder(
      folders, normalizePulledNoteFolders(folders, pulledNotes, new Set()),
      async () => { /* Remote tombstone for an absent folder is intentionally not materialized. */ },
      async (note) => {
        if (note.folder_id !== null) throw new Error("active folder not found");
        restored.push(note.content);
      },
      () => {},
    );
    expect(restored).toEqual(["Keep this note"]);
    expect(pulledNotes[0].folder_id).toBe("deleted-folder");
  });

  it("preserves unknown parents and tombstone metadata for strict scoped import validation", () => {
    const notes = [
      { folder_id: "unknown-folder", deleted_at: null },
      { folder_id: "deleted-folder", deleted_at: "2026-11-05T10:00:00Z" },
    ];
    expect(normalizePulledNoteFolders([{ id: "deleted-folder", deleted_at: "2026-11-05T10:00:00Z" }], notes, new Set())).toEqual(notes);
  });

  it("makes folders available before importing notes on a fresh profile", async () => {
    // Folders are persisted before the strict note importer runs.
    const folders = new Set<string>();
    const notes: string[] = [];
    const errors: unknown[] = [];
    await applyContentInDependencyOrder(
      [{ id: "folder" }], [{ id: "note", folderId: "folder" }],
      async (folder) => { folders.add(folder.id); },
      async (note) => {
        if (!folders.has(note.folderId)) throw new Error("Folder not found");
        notes.push(note.id);
      },
      (_kind, _id, error) => { errors.push(error); },
    );
    expect(notes).toEqual(["note"]);
    expect(errors).toEqual([]);
  });

  it("keeps a parent that survived folder LWW against an older remote tombstone", () => {
    const note = { folder_id: "folder", deleted_at: null, content: "still filed" };
    const prepared = normalizePulledNoteFolders(
      [{ id: "folder", deleted_at: "2026-11-01T10:00:00Z" }], [note], new Set(["folder"]),
    );
    expect(prepared).toEqual([note]);
  });

  it("reports a corrupt row while still applying the remaining rows", async () => {
    const notes: string[] = [];
    const failedIds: string[] = [];
    await applyContentInDependencyOrder(
      [{ id: "broken" }], [{ id: "independent" }],
      async () => { throw new Error("corrupt folder"); },
      async (note) => { notes.push(note.id); },
      (_kind, id) => { failedIds.push(id); },
    );
    expect(notes).toEqual(["independent"]);
    expect(failedIds).toEqual(["broken"]);
  });
});
