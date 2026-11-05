import { useCallback, useEffect, useRef, useState } from "react";
import { Store } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";
import type { NoteMeta } from "@/hooks/useNotes";
import {
  detachNote,
  forgetNote,
  mergeDetachedAtLoad,
  reattachNote,
  type NotesTabsState,
} from "@/lib/notes-window/tab-transitions";

interface UseNotesWorkflowOptions {
  createNote: (folderId?: string | null) => Promise<NoteMeta>;
  deleteNote: (id: string) => Promise<void>;
  notes: NoteMeta[];
  notesLoaded: boolean;
}

interface PersistedTabState {
  openNoteIds: string[];
  activeNoteId: string | null;
  /** Notes currently open in their own detached window. Optional so tab
   *  stores written before this feature keep loading. */
  detachedNoteIds?: string[];
}

const STORE_KEY = "tabs";
const SAVE_DEBOUNCE_MS = 300;

const EMPTY_TABS_STATE: NotesTabsState = {
  openNoteIds: [],
  activeNoteId: null,
  detachedNoteIds: [],
};

let tabStore: Store | null = null;
async function getTabStore(): Promise<Store> {
  if (!tabStore) {
    const path = await invoke<string>("get_active_profile_notes_tabs_path");
    tabStore = await Store.load(path);
  }
  return tabStore;
}

/**
 * Manages the open-tabs state for the docked notes editor — which notes are
 * open as tabs, which one is active — plus the registry of notes detached
 * into their own OS window (spec 2026-07-24-detachable-notes-design §4).
 *
 * The whole state lives in ONE object mutated exclusively through functional
 * updates built on the pure transitions: every transition reads the TRUE
 * latest state even when scheduled after an await (Rust IPC round-trip), so
 * two in-flight detaches can never clobber each other's registry entries.
 *
 * The state is persisted per profile; at load, detached notes come back as
 * tabs (the windows themselves are never restored across restarts).
 */
export function useNotesWorkflow({
  createNote,
  deleteNote,
  notes,
  notesLoaded,
}: UseNotesWorkflowOptions) {
  const [tabsState, setTabsState] = useState<NotesTabsState>(EMPTY_TABS_STATE);
  const hasLoadedRef = useRef(false);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Fresh mirror for synchronous READS inside async handlers — never used
  // for updates (those always go through the functional setter).
  const tabsStateRef = useRef(tabsState);
  tabsStateRef.current = tabsState;

  // Load persisted tabs once the notes list is available. Detached notes are
  // merged back into the tab strip (idempotent — covers restart and crash).
  useEffect(() => {
    if (!notesLoaded || hasLoadedRef.current) return;
    let cancelled = false;
    (async () => {
      try {
        const store = await getTabStore();
        const persisted = await store.get<PersistedTabState>(STORE_KEY);
        if (cancelled) return;
        if (persisted) {
          const validIds = new Set(notes.map((n) => n.id));
          setTabsState(mergeDetachedAtLoad(persisted, validIds));
        }
      } catch (error) {
        console.error("Failed to load persisted note tabs:", error);
      } finally {
        if (!cancelled) hasLoadedRef.current = true;
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [notesLoaded, notes]);

  // Persist tab state on every change, debounced.
  useEffect(() => {
    if (!hasLoadedRef.current) return;
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(async () => {
      try {
        const store = await getTabStore();
        await store.set(STORE_KEY, {
          openNoteIds: tabsState.openNoteIds,
          activeNoteId: tabsState.activeNoteId,
          detachedNoteIds: tabsState.detachedNoteIds,
        });
        await store.save();
      } catch (error) {
        console.error("Failed to persist note tabs:", error);
      }
    }, SAVE_DEBOUNCE_MS);
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    };
  }, [tabsState]);

  const setActiveNoteId = useCallback((id: string | null) => {
    setTabsState((prev) => ({ ...prev, activeNoteId: id }));
  }, []);

  const handleCreateNote = useCallback(
    async (folderId: string | null = null) => {
      const note = await createNote(folderId);
      setTabsState((prev) => ({
        ...prev,
        openNoteIds: [...prev.openNoteIds, note.id],
        activeNoteId: note.id,
      }));
    },
    [createNote],
  );

  const handleOpenNote = useCallback((note: NoteMeta) => {
    setTabsState((prev) => ({
      ...prev,
      openNoteIds: prev.openNoteIds.includes(note.id)
        ? prev.openNoteIds
        : [...prev.openNoteIds, note.id],
      activeNoteId: note.id,
    }));
  }, []);

  /** Closing a plain tab and forgetting a note share the same transition —
   *  forgetNote's registry filter is a no-op for a non-detached note. */
  const handleCloseNoteTab = useCallback((id: string) => {
    setTabsState((prev) => forgetNote(prev, id));
  }, []);

  /** Detach: create/focus the OS window first; only update the tab state
   *  when the window actually opened. */
  const handleDetachNote = useCallback(async (id: string, atCursor = false) => {
    try {
      await invoke("open_note_window", { noteId: id, atCursor });
    } catch (error) {
      console.error("Failed to open note window:", error);
      return;
    }
    setTabsState((prev) => detachNote(prev, id));
  }, []);

  /** Rust `note-window-closed` (native X or any window death): restore the
   *  tab silently — the main window is NOT shown. No-op when the id isn't in
   *  the registry (delete / explicit reattach removed it first). */
  const handleNoteWindowClosed = useCallback((id: string) => {
    setTabsState((prev) => reattachNote(prev, id, { activate: false }));
  }, []);

  /** Explicit « réattacher » button: restore + activate the tab. The caller
   *  (bridge) also shows the main window and closes the note window. */
  const handleReattachNote = useCallback((id: string) => {
    setTabsState((prev) => reattachNote(prev, id, { activate: true }));
  }, []);

  const handleDeleteNote = useCallback(
    async (id: string) => {
      // A dirty owner rejects deletion. Keep its window, tab and buffer intact
      // until the native ownership barrier and durable delete have succeeded.
      const wasDetached = tabsStateRef.current.detachedNoteIds.includes(id);
      await deleteNote(id);
      // Delete committed: forget before requesting close so its completion
      // cannot resurrect the deleted note as a tab.
      setTabsState((prev) => forgetNote(prev, id));
      if (wasDetached) {
        try {
          await invoke("close_note_window", { noteId: id });
        } catch (error) {
          console.error("Failed to close note window:", error);
        }
      }
    },
    [deleteNote],
  );

  return {
    openNoteIds: tabsState.openNoteIds,
    activeNoteId: tabsState.activeNoteId,
    detachedNoteIds: tabsState.detachedNoteIds,
    setActiveNoteId,
    handleCreateNote,
    handleOpenNote,
    handleCloseNoteTab,
    handleDeleteNote,
    handleDetachNote,
    handleNoteWindowClosed,
    handleReattachNote,
  };
}
