import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { type NoteData, type NoteMeta } from "@/hooks/useNotes";
import { type FolderMeta } from "@/hooks/useFolders";
import { createNoteMetadataRefresh, createScopedMetadataRefresh, mergeNoteMetadata, type MetadataChange, type MetadataSession } from "@/lib/notes/metadata-refresh";

/**
 * Data layer of a detached note window (spec §5): loads the note + the
 * notes/folders lists (for wiki-links and the breadcrumb), saves to disk
 * through the ownership service, and talks to main with profile-bound events.
 * NO sync state lives here — the main window owns the queue.
 */
export function useDetachedNote(noteId: string) {
  const [{ meta, notes, folders, loadFailed }, setMetadata] = useState<{
    meta: NoteMeta | null; notes: NoteMeta[]; folders: FolderMeta[]; loadFailed: boolean;
  }>({ meta: null, notes: [], folders: [], loadFailed: false });
  const markNoteChanges = useRef<(ids: string[]) => void>(() => {});
  const sessionRef = useRef<Promise<{profile_id:string;session_generation:number}> | null>(null);
  const getSession = useCallback(() => sessionRef.current ??= invoke("get_profile_session"),[]);
  const emitScoped = useCallback((name:string,payload:Record<string,unknown>) => {
    void getSession().then(session => emit(name,{...payload,...session})).catch(error => console.error("Detached note event rejected",error));
  },[getSession]);

  useEffect(() => {
    let disposed = false;
    let fullLoad = 0;
    // Only retain changes for in-flight full loads. Targeted events never cause
    // a full scan, and unrelated initial/wiki-link entries still get installed.
    const snapshots = new Set<{ noteIds: Set<string>; foldersChanged: boolean }>();
    const changedNotes = (ids: string[]) => {
      for (const snapshot of snapshots) ids.forEach(id => snapshot.noteIds.add(id));
    };
    const changedFolders = () => {
      for (const snapshot of snapshots) snapshot.foldersChanged = true;
    };
    markNoteChanges.current = changedNotes;
    const refreshLists = async () => {
      const order = ++fullLoad;
      const snapshot = { noteIds: new Set<string>(), foldersChanged: false };
      snapshots.add(snapshot);
      try {
        const captured = await getSession();
        if (disposed) return;
        const [allNotes, allFolders] = await Promise.all([
          invoke<NoteMeta[]>("list_notes"),
          invoke<FolderMeta[]>("list_folders"),
        ]);
        const current = await invoke<MetadataSession>("get_profile_session");
        if (disposed || order !== fullLoad || current.profile_id !== captured.profile_id
          || current.session_generation !== captured.session_generation) return;
        setMetadata(previous => {
          if (disposed || order !== fullLoad) return previous;
          const ids = [...snapshot.noteIds];
          const merged = mergeNoteMetadata(allNotes, ids, previous.notes);
          const own = merged.find(note => note.id === noteId && !note.deletedAt) ?? null;
          return { notes: merged, folders: snapshot.foldersChanged ? previous.folders : allFolders,
            meta: own, loadFailed: own === null };
        });
      } catch (error) {
        if (!disposed && order === fullLoad) {
          console.error("[note-window] failed to refresh lists:", error);
          setMetadata(previous => ({ ...previous, loadFailed: true }));
        }
      } finally {
        snapshots.delete(snapshot);
      }
    };
    void refreshLists();
    const focusSubscription = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused && !disposed) void refreshLists();
    });
    let noteRefresh: ReturnType<typeof createNoteMetadataRefresh> | undefined;
    let folderRefresh: ReturnType<typeof createScopedMetadataRefresh<FolderMeta>> | undefined;
    const subscriptions = getSession().then<Array<() => void>>(session => {
      if (disposed) return [];
      const matches = (change: Omit<MetadataChange, 'note_id'>) => !disposed
        && change.profile_id === session.profile_id
        && (change.session_generation === undefined || change.session_generation === session.session_generation);
      noteRefresh = createNoteMetadataRefresh(session, (ids, values) => {
        changedNotes(ids);
        setMetadata(previous => {
          if (disposed) return previous;
          const merged = mergeNoteMetadata(previous.notes, ids, values);
          const own = ids.includes(noteId) ? values.find(note => note.id === noteId && !note.deletedAt) ?? null : previous.meta;
          return { ...previous, notes: merged, meta: own, loadFailed: ids.includes(noteId) ? own === null : previous.loadFailed };
        });
      }, error => console.warn("[note-window] metadata refresh failed:", error));
      folderRefresh = createScopedMetadataRefresh<FolderMeta>({
        session,
        currentSession: () => invoke<MetadataSession>("get_profile_session"),
        fetch: () => invoke<FolderMeta[]>("list_folders"),
        apply: (_ids, values) => {
          changedFolders();
          setMetadata(previous => disposed ? previous : ({ ...previous, folders: values }));
        },
        onError: error => console.warn("[note-window] folder refresh failed:", error),
      });
      return Promise.all([
        listen<MetadataChange>('note-service-changed', event => {
          if (!matches(event.payload) || !event.payload.note_id) return;
          noteRefresh?.enqueue(event.payload);
        }),
        listen<Omit<MetadataChange, 'note_id'>>('folder-service-changed', event => {
          if (!matches(event.payload)) return;
          folderRefresh?.enqueue({ ...event.payload, note_id: 'folders' });
        }),
        listen<{ meta: NoteMeta } & MetadataSession>('note-meta-updated', event => {
          if (!matches(event.payload) || event.payload.session_generation !== session.session_generation
            || event.payload.meta.id !== noteId) return;
          changedNotes([noteId]);
          const own = event.payload.meta;
          setMetadata(previous => ({ ...previous, meta: own.deletedAt ? null : own,
            loadFailed: Boolean(own.deletedAt), notes: mergeNoteMetadata(previous.notes, [noteId], [own]) }));
        }),
      ]);
    });
    return () => {
      disposed = true;
      snapshots.clear();
      if (markNoteChanges.current === changedNotes) markNoteChanges.current = () => {};
      noteRefresh?.dispose();
      folderRefresh?.dispose();
      void focusSubscription.then(unlisten => unlisten()).catch(() => {});
      void subscriptions.then(listeners => listeners.forEach(unlisten => unlisten())).catch(() => {});
    };
  }, [getSession, noteId]);

  // Keep the OS window title in sync with the note title.
  useEffect(() => {
    const title = meta?.title?.trim();
    void getCurrentWindow().setTitle(title ? `${title} — Lexena` : "Lexena");
  }, [meta?.title]);

  const readNote = useCallback(
    (id: string) => invoke<NoteData>("read_note", { id }),
    [],
  );

  /** The ownership service has already persisted content and its durable outbox. */
  const applySavedNoteMeta = useCallback((updated:NoteMeta) => {
    markNoteChanges.current([updated.id]);
    setMetadata(previous => ({ ...previous, meta: updated.deletedAt ? null : updated,
      loadFailed: Boolean(updated.deletedAt), notes: mergeNoteMetadata(previous.notes, [updated.id], [updated]) }));
    emitScoped("note-detached-updated", {id:updated.id,title:updated.title,updatedAt:updated.updatedAt});
  },[emitScoped]);

  const requestReattach = useCallback(() => {
    emitScoped("note-reattach-request", { id: noteId });
  }, [noteId,emitScoped]);

  const requestDelete = useCallback(() => {
    emitScoped("note-detached-delete-request", { id: noteId });
  }, [noteId,emitScoped]);

  const requestToggleLocalOnly = useCallback(() => {
    emitScoped("note-toggle-local-only-request", { id: noteId });
  }, [noteId,emitScoped]);

  const openNoteInMain = useCallback((id: string) => {
    emitScoped("note-open-request", { id });
  }, [emitScoped]);

  return {
    meta,
    notes,
    folders,
    loadFailed,
    readNote,
    applySavedNoteMeta,
    requestReattach,
    requestDelete,
    requestToggleLocalOnly,
    openNoteInMain,
  };
}
