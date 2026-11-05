// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { EditorContext, EditorRead } from "@/lib/notes/editor-ownership";
import type { NoteMeta } from "./useNotes";

const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.hoisted(() => { vi.stubGlobal("localStorage", { getItem: () => null }); });
vi.mock("@tauri-apps/api/core", () => ({ invoke: ipc.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    ipc.handlers.set(name, handler);
    return () => ipc.handlers.delete(name);
  }),
}));

// Keep the real TipTap instance and extensions: setEditable emits update by
// default, even though its transaction has not changed the document.
import { useNotesEditorInstance } from "./useNotesEditorInstance";

const session = { profile_id: "work", session_generation: 4 };
const note = (id: string): NoteMeta => ({
  id, title: id.toUpperCase(), createdAt: "now", updatedAt: "now", favorite: false, order: 0,
});
let disk: Map<string, { content: string; revision: string }>;
function read(context: EditorContext): EditorRead {
  const stored = disk.get(context.note_id)!;
  return { context: { ...context, loaded_revision: stored.revision }, note: { meta: { ...note(context.note_id) }, content: stored.content } };
}
function options(activeNoteId: string) {
  return { activeNoteId, openNotes: [note("a"), note("b")], readNote: vi.fn(), onNoteSaved: vi.fn(),
    getNoteLinkRefs: () => ({ notes: [note("a"), note("b")], activeNoteId }) };
}
beforeEach(() => {
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value), removeItem: (key: string) => storage.delete(key) });
  ipc.handlers.clear();
  vi.clearAllMocks();
  disk = new Map([ ["a", { content: "<p>Note A</p>", revision: "r1" }], ["b", { content: "<p>Note B</p>", revision: "r1" }] ]);
  ipc.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_profile_session") return session;
    if (command === "register_note_editor") return read({ ...session, note_id: args!.noteId as string, editor_id: args!.editorId as string, loaded_revision: "r1" });
    if (command === "read_editor_note") return read(args!.context as EditorContext);
    if (command === "save_editor_note") {
      const context = args!.context as EditorContext;
      if (context.loaded_revision !== disk.get(context.note_id)!.revision) throw Error("REVISION_CONFLICT");
      disk.set(context.note_id, { content: args!.content as string, revision: "saved" });
      return read(context);
    }
  });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("note ownership with real TipTap lifecycle", () => {
  it("opens and switches clean notes without creating drafts or saving unchanged content", async () => {
    const { result, rerender } = renderHook(({ id }) => useNotesEditorInstance(options(id)), { initialProps: { id: "a" } });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("a"));
    expect(localStorage.getItem("lexena:note-draft:work:a")).toBeNull();
    await act(async () => { await result.current.flushSave(); });
    expect(ipc.invoke.mock.calls.filter(([command]) => command === "save_editor_note" || command === "begin_note_edit")).toHaveLength(0);
    rerender({ id: "b" });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("b"));
    expect(result.current.editor!.getHTML()).toBe("<p>Note B</p>");
    expect(result.current.saveError).toBeNull();
    expect(ipc.invoke.mock.calls.filter(([command]) => command === "save_editor_note")).toHaveLength(0);
  });

  it("refreshes an externally changed clean note and still allows switching tabs", async () => {
    const { result, rerender } = renderHook(({ id }) => useNotesEditorInstance(options(id)), { initialProps: { id: "a" } });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("a"));
    await act(async () => { await result.current.flushSave(); });
    disk.set("a", { content: "<p>Updated through MCP</p>", revision: "mcp-r2" });
    await act(async () => { ipc.handlers.get("note-service-changed")!({ payload: { profile_id: "work", note_id: "a" } }); });
    await waitFor(() => expect(result.current.editor!.getHTML()).toBe("<p>Updated through MCP</p>"));
    expect(result.current.saveError).toBeNull();
    expect(result.current.editor!.isEditable).toBe(true);
    await act(async () => { await result.current.flushSave(); });
    rerender({ id: "b" });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("b"));
    expect(result.current.editor!.getHTML()).toBe("<p>Note B</p>");
  });

  it("preserves real edits through the dirty barrier and saves before switching", async () => {
    const { result, rerender } = renderHook(({ id }) => useNotesEditorInstance(options(id)), { initialProps: { id: "a" } });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("a"));
    act(() => { result.current.editor!.commands.insertContentAt(7, " — draft"); });
    expect(localStorage.getItem("lexena:note-draft:work:a")).toContain("draft");
    await act(async () => { await result.current.flushSave(); });
    expect(disk.get("a")!.content).toBe("<p>Note A — draft</p>");
    expect(localStorage.getItem("lexena:note-draft:work:a")).toBeNull();
    rerender({ id: "b" });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("b"));
    expect(result.current.saveError).toBeNull();
    expect(disk.get("a")!.content).toBe("<p>Note A — draft</p>");
  });

  it("recovers a redundant draft left by an earlier false update without rewriting the note", async () => {
    localStorage.setItem("lexena:note-draft:work:a", JSON.stringify({ content: "<p>Note A</p>", revision: "older-revision" }));
    const { result, rerender } = renderHook(({ id }) => useNotesEditorInstance(options(id)), { initialProps: { id: "a" } });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("a"));
    expect(result.current.saveError).toBeNull();
    expect(localStorage.getItem("lexena:note-draft:work:a")).toBeNull();
    rerender({ id: "b" });
    await waitFor(() => expect(result.current.loadedNoteId).toBe("b"));
    expect(ipc.invoke.mock.calls.filter(([command]) => command === "save_editor_note")).toHaveLength(0);
  });
});
