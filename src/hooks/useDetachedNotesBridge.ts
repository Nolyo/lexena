import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

export interface DetachedNotesBridgeHandlers {
  /** Rust `note-window-closed` — silent tab restore (native X). */
  onWindowClosed: (id: string) => void;
  /** « Réattacher » button — restore + activate + show main. */
  onReattachRequest: (id: string) => void;
  /** Wiki-link clicked in a detached window. */
  onOpenRequest: (id: string) => void;
  /** Delete confirmed in a detached window — canonical delete flow. */
  onDeleteRequest: (id: string) => void;
  /** A detached window saved the note to disk. */
  onDetachedUpdated: (payload: { id: string; title: string; updatedAt: string }) => void;
  /** Local-only toggle clicked in a detached window. */
  onToggleLocalOnlyRequest: (id: string) => void;
}

/**
 * Main-window side of the detached note windows (spec §5): subscribes once to
 * every event a detached window (or Rust) can emit and routes it to the
 * workflow. Handlers live in a ref so the listeners registered at mount
 * always call the freshest closures.
 */
export function useDetachedNotesBridge(handlers: DetachedNotesBridgeHandlers) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    let disposed = false;
    let session: {profile_id:string;session_generation:number} | null = null;
    const ready = invoke<{profile_id:string;session_generation:number}>("get_profile_session").then(value => {session=value;});
    const belongs = (payload:{profile_id?:string;session_generation?:number}) => !disposed && session && payload.profile_id === session.profile_id && payload.session_generation === session.session_generation;
    type Scoped = {profile_id:string;session_generation:number};
    const unlistenPromises = [
      ready.then(() => listen<Scoped & { noteId: string }>("note-window-closed", (e) => {
        if (belongs(e.payload)) handlersRef.current.onWindowClosed(e.payload.noteId);
      })),
      ready.then(() => listen<Scoped & { id: string }>("note-reattach-request", (e) => {
        if (belongs(e.payload)) handlersRef.current.onReattachRequest(e.payload.id);
      })),
      ready.then(() => listen<Scoped & { id: string }>("note-open-request", (e) => {
        if (belongs(e.payload)) handlersRef.current.onOpenRequest(e.payload.id);
      })),
      ready.then(() => listen<Scoped & { id: string }>("note-detached-delete-request", (e) => {
        if (belongs(e.payload)) handlersRef.current.onDeleteRequest(e.payload.id);
      })),
      ready.then(() => listen<Scoped & { id: string; title: string; updatedAt: string }>(
        "note-detached-updated",
        (e) => {if (belongs(e.payload)) handlersRef.current.onDetachedUpdated(e.payload);},
      )),
      ready.then(() => listen<Scoped & { id: string }>("note-toggle-local-only-request", (e) => {
        if (belongs(e.payload)) handlersRef.current.onToggleLocalOnlyRequest(e.payload.id);
      })),
    ];
    return () => {
      disposed = true;
      for (const p of unlistenPromises) void p.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);
}
