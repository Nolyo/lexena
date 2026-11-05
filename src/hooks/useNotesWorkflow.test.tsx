// @vitest-environment jsdom
import { cleanup, renderHook, waitFor, act } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
const ipc = vi.hoisted(() => ({invoke:vi.fn()}));
vi.mock("@tauri-apps/api/core", () => ipc);
vi.mock("@tauri-apps/plugin-store", () => ({Store:{load:async()=>({get:async()=>({openNoteIds:[],activeNoteId:null,detachedNoteIds:["a"]}),set:async()=>{},save:async()=>{}})}}));
import { useNotesWorkflow } from "./useNotesWorkflow";
const note={id:"a",title:"A",createdAt:"now",updatedAt:"now",favorite:false,order:0};
beforeEach(()=>{vi.clearAllMocks();ipc.invoke.mockResolvedValue("tabs.json");});
afterEach(cleanup);
it("retains the detached registry and does not request window close when dirty-owner delete rejects",async()=>{
  const deleteNote=vi.fn(async()=>{throw Error("EDITOR_BUSY");});
  const {result}=renderHook(()=>useNotesWorkflow({createNote:vi.fn(),deleteNote,notes:[note],notesLoaded:true}));
  await waitFor(()=>expect(result.current.openNoteIds).toContain("a"));
  // Restart restoration opens detached IDs as tabs; detach again for this session.
  await act(async()=>{await result.current.handleDetachNote("a");});
  await act(async()=>{await expect(result.current.handleDeleteNote("a")).rejects.toThrow("EDITOR_BUSY");});
  expect(result.current.detachedNoteIds).toContain("a");
  expect(ipc.invoke.mock.calls.some(([command])=>command==="close_note_window")).toBe(false);
});
it("keeps tabs while delete is pending and forgets only after durable success",async()=>{
  let finish!:()=>void;
  const deleteNote=vi.fn(()=>new Promise<void>(done=>{finish=done;}));
  const {result}=renderHook(()=>useNotesWorkflow({createNote:vi.fn(),deleteNote,notes:[note],notesLoaded:true}));
  await waitFor(()=>expect(result.current.openNoteIds).toContain("a"));
  let deleting!:Promise<void>;
  act(()=>{deleting=result.current.handleDeleteNote("a");});
  expect(result.current.openNoteIds).toContain("a");
  await act(async()=>{finish();await deleting;});
  expect(result.current.openNoteIds).not.toContain("a");
});
