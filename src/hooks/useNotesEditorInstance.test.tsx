// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { NoteMeta } from "./useNotes";

const state = vi.hoisted(() => {
  const editor = {
    setEditable: vi.fn(), getHTML: () => state.html,
    commands: {setContent: vi.fn((content:string) => {state.html=content;}),focus:vi.fn()},
  };
  return {html:"",options:null as null | {onUpdate:(arg:{editor:typeof editor})=>void},editor,handlers:new Map<string,(event:{payload:unknown})=>void>()};
});
const ipc = vi.hoisted(() => ({invoke:vi.fn()}));
vi.mock("@tauri-apps/api/core",() => ipc);
vi.mock("@tauri-apps/api/event",() => ({listen:vi.fn(async(name:string,handler:(event:{payload:unknown})=>void)=>{state.handlers.set(name,handler);return ()=>state.handlers.delete(name);})}));
vi.mock("@tiptap/react",() => ({useEditor:(options:typeof state.options)=>{state.options=options;return state.editor;},ReactNodeViewRenderer:vi.fn()}));
vi.mock("@/hooks/useNotes",() => ({deriveTitle:(content:string)=>content}));
vi.mock("@/i18n",() => ({default:{t:(_key:string,options?:{defaultValue?:string})=>options?.defaultValue??_key}}));
import { useNotesEditorInstance } from "./useNotesEditorInstance";

const session={profile_id:"work",session_generation:4};
const meta:NoteMeta={id:"a",title:"A",createdAt:"now",updatedAt:"now",favorite:false,order:0};
function read(noteId:string,editorId:string){return {context:{...session,note_id:noteId,editor_id:editorId,loaded_revision:"r1"},note:{meta:{...meta,id:noteId},content:`<p>${noteId}</p>`}};}
function options(activeNoteId:string|null){return {activeNoteId,openNotes:[meta],readNote:vi.fn(),onNoteSaved:vi.fn(),getNoteLinkRefs:()=>({notes:[meta],activeNoteId})};}
beforeEach(()=>{
  const storage = new Map<string,string>();
  vi.stubGlobal("localStorage",{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value),removeItem:(key:string)=>storage.delete(key),clear:()=>storage.clear()});
  state.html="";state.handlers.clear();localStorage.clear();vi.clearAllMocks();
  ipc.invoke.mockImplementation(async(command:string,args?:Record<string,unknown>)=>{
    if(command==="get_profile_session")return session;
    if(command==="register_note_editor")return read(args!.noteId as string,args!.editorId as string);
    if(command==="save_editor_note")return {context:{...(args!.context as object),loaded_revision:"r2"},note:{meta,content:args!.content}};
    return undefined;
  });
});
afterEach(cleanup);
describe("shared main/detached editor hook",()=>{
  it("reopens A after A to B to A while the first owner end is pending",async()=>{
    let finishEnd!:()=>void;
    const normal=ipc.invoke.getMockImplementation()!;
    ipc.invoke.mockImplementation(async(command:string,args?:Record<string,unknown>)=>{
      if(command==="end_note_edit" && !finishEnd) return new Promise<void>(done=>{finishEnd=done;});
      return normal(command,args);
    });
    const {result,rerender}=renderHook(({id})=>useNotesEditorInstance(options(id)),{initialProps:{id:"a"}});
    await waitFor(()=>expect(result.current.loadedNoteId).toBe("a"));
    rerender({id:"b"});
    await waitFor(()=>expect(finishEnd).toBeTypeOf("function"));
    rerender({id:"a"});
    await act(async()=>{finishEnd();});
    await waitFor(()=>expect(result.current.isLoadingContent).toBe(false));
    expect(ipc.invoke.mock.calls.filter(([cmd,args])=>cmd==="register_note_editor" && args.noteId==="a")).toHaveLength(2);
    expect(state.editor.setEditable).toHaveBeenLastCalledWith(true, false);
    act(()=>{state.html="returned draft";state.options!.onUpdate({editor:state.editor});});
    await act(async()=>{await result.current.flushSave();});
    expect(ipc.invoke).toHaveBeenCalledWith("save_editor_note",expect.objectContaining({content:"returned draft",context:expect.objectContaining({note_id:"a"})}));
  });
  it.each(["REVISION_CONFLICT","STORAGE_ERROR"])("exposes A's retained draft after a pending close save rejects with %s during A to B to A",async(errorCode)=>{
    let rejectSave!: (error:Error)=>void;
    const normal=ipc.invoke.getMockImplementation()!;
    ipc.invoke.mockImplementation(async(command:string,args?:Record<string,unknown>)=>{
      if(command==="save_editor_note")return new Promise((_resolve,reject)=>{rejectSave=reject;});
      return normal(command,args);
    });
    const {result,rerender}=renderHook(({id})=>useNotesEditorInstance(options(id)),{initialProps:{id:"a"}});
    await waitFor(()=>expect(result.current.loadedNoteId).toBe("a"));
    act(()=>{state.html="<p>retained recovery draft</p>";state.options!.onUpdate({editor:state.editor});});
    rerender({id:"b"});
    await waitFor(()=>expect(rejectSave).toBeTypeOf("function"));
    expect(result.current.isLoadingContent).toBe(true);
    rerender({id:"a"});
    await act(async()=>{rejectSave(Error(errorCode));});
    await waitFor(()=>expect(result.current.isLoadingContent).toBe(false));
    expect(result.current.loadedNoteId).toBe("a");
    expect(result.current.saveError).toMatch(/copiez votre texte/i);
    expect(state.html).toBe("<p>retained recovery draft</p>");
    expect(state.editor.setEditable).toHaveBeenLastCalledWith(false, false);
    expect(localStorage.getItem("lexena:note-draft:work:a")).toBe(JSON.stringify({content:state.html,revision:"r1"}));
    expect(state.editor.commands.setContent).toHaveBeenCalledTimes(1);
    expect(ipc.invoke.mock.calls.filter(([command])=>command==="register_note_editor")).toHaveLength(1);
    expect(ipc.invoke.mock.calls.filter(([command])=>command==="save_editor_note")).toHaveLength(1);
    expect(ipc.invoke.mock.calls.filter(([command])=>command==="read_editor_note" || command==="end_note_edit")).toHaveLength(0);
    // The failed owner stays registered: explicit close must still report its failure.
    await expect(result.current.closeEditor()).rejects.toThrow(errorCode);
    expect(result.current.loadedNoteId).toBe("a");
    expect(state.html).toBe("<p>retained recovery draft</p>");
    expect(ipc.invoke.mock.calls.filter(([command])=>command==="save_editor_note")).toHaveLength(1);
  });
  it("sends a synchronous dirty barrier then one revision-bound save, without a second update_note",async()=>{
    const props=options("a");const {result}=renderHook(()=>useNotesEditorInstance(props));
    await waitFor(()=>expect(result.current.loadedNoteId).toBe("a"));
    act(()=>{state.html="<p>draft</p>";state.options!.onUpdate({editor:state.editor});});
    await act(async()=>{await result.current.flushSave();});
    const commands=ipc.invoke.mock.calls.map(call=>call[0]);
    expect(commands.indexOf("begin_note_edit")).toBeLessThan(commands.indexOf("save_editor_note"));
    expect(commands).not.toContain("update_note");expect(props.readNote).not.toHaveBeenCalled();
    expect(ipc.invoke).toHaveBeenCalledWith("save_editor_note",expect.objectContaining({context:expect.objectContaining({...session,note_id:"a",loaded_revision:"r1"}),content:"<p>draft</p>"}));
    expect(props.onNoteSaved).toHaveBeenCalledWith(meta);
  });
  it("keeps the visible draft and rejects close or tab switch when save fails",async()=>{
    const {result,rerender}=renderHook(({id})=>useNotesEditorInstance(options(id)),{initialProps:{id:"a"}});
    await waitFor(()=>expect(result.current.loadedNoteId).toBe("a"));
    ipc.invoke.mockImplementation(async(command:string)=>{if(command==="save_editor_note")throw Error("REVISION_CONFLICT");});
    act(()=>{state.html="my unsaved draft";state.options!.onUpdate({editor:state.editor});});
    rerender({id:"b"});
    await waitFor(()=>expect(result.current.saveError).toBeTruthy());
    expect(state.html).toBe("my unsaved draft");expect(result.current.loadedNoteId).toBe("a");
    await expect(result.current.closeEditor()).rejects.toThrow("REVISION_CONFLICT");
    expect(localStorage.getItem("lexena:note-draft:work:a")).toContain("my unsaved draft");
  });
  it("does not load a delayed registration after the active note changed",async()=>{
    let resolve!: (data:ReturnType<typeof read>)=>void;
    ipc.invoke.mockImplementation(async(command:string,args?:Record<string,unknown>)=>{
      if(command==="get_profile_session")return session;
      if(command==="register_note_editor"){
        if(args!.noteId==="a")return new Promise<ReturnType<typeof read>>(done=>{resolve=done;});
        return read(args!.noteId as string,args!.editorId as string);
      }
    });
    const {result,rerender}=renderHook(({id})=>useNotesEditorInstance(options(id)),{initialProps:{id:"a"}});
    await waitFor(()=>expect(resolve).toBeTypeOf("function"));rerender({id:"b"});
    await act(async()=>{resolve(read("a","old-editor"));});
    await waitFor(()=>expect(result.current.loadedNoteId).toBe("b"));
    expect(state.html).toBe("<p>b</p>");
    expect(ipc.invoke).toHaveBeenCalledWith("end_note_edit",{context:expect.objectContaining({...session,note_id:"a",editor_id:"old-editor"})});
  });
});
