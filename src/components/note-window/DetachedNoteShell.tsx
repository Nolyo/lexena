import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Toaster } from "sonner";
import { ArrowLeftToLine, Pin, PinOff } from "lucide-react";
import { bootstrapSecondaryWindow } from "@/lib/window-bootstrap";
import { useDetachedNote } from "@/hooks/useDetachedNote";
import { useNotesEditorInstance } from "@/hooks/useNotesEditorInstance";
import { useLinkEditor } from "@/hooks/useLinkEditor";
import { createNoteSynced } from "@/lib/sync/notes-store";
import { getQueueSession } from "@/lib/sync/queue";
import { type NoteMeta } from "@/hooks/useNotes";
import { type Theme, DEFAULT_THEME } from "@/lib/theme";
import { NoteLinkProvider } from "@/components/notes/NotesEditor/NoteLinkContext";
import { NotesEditorContent } from "@/components/notes/NotesEditor/NotesEditorContent";
import { NotesEditorFooter } from "@/components/notes/NotesEditor/NotesEditorFooter";
import { ConfirmDeleteDialog } from "@/components/notes/ConfirmDeleteDialog";
import { BrokenNoteLinkDialog } from "@/components/notes/NotesEditor/BrokenNoteLinkDialog";

/**
 * Detached note window: native title bar, a thin toolbar (pin + reattach),
 * the full TipTap editor, and the standard footer without AI/share
 * (spec §7). One window = one note; the tab lives here, not in the main
 * window, until the window closes (close = reattach).
 */
export function DetachedNoteShell({ noteId }: { noteId: string }) {
  const { t } = useTranslation();
  const [pinned, setPinned] = useState(false);
  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);
  const [theme, setTheme] = useState<Theme>(DEFAULT_THEME);
  const [brokenDialog, setBrokenDialog] = useState<{
    title: string;
    onResolved: (newId: string) => void;
  } | null>(null);

  const {
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
  } = useDetachedNote(noteId);

  // Theme + language bootstrap (shared with the mini window). The local
  // `theme` state only feeds the Toaster — applyTheme handles the DOM.
  useEffect(() => {
    let cleanup: (() => void) | null = null;
    let unlistenTheme: (() => void) | null = null;
    void (async () => {
      const bootstrap = await bootstrapSecondaryWindow();
      cleanup = bootstrap.unlisten;
      if (bootstrap.settings?.theme === "light" || bootstrap.settings?.theme === "dark") {
        setTheme(bootstrap.settings.theme);
      }
      unlistenTheme = await listen<Theme>("theme-changed", (event) => {
        if (event.payload === "light" || event.payload === "dark") {
          setTheme(event.payload);
        }
      });
    })();
    return () => {
      cleanup?.();
      unlistenTheme?.();
    };
  }, []);

  const openNotes = useMemo(() => (meta ? [meta] : []), [meta]);

  const linkRefsRef = useRef({ notes, activeNoteId: noteId });
  linkRefsRef.current = { notes, activeNoteId: noteId };
  const getNoteLinkRefs = useRef(() => linkRefsRef.current).current;

  const { editor, isLoadingContent, loadedNoteId, flushSave, closeEditor, saveError } =
    useNotesEditorInstance({
      openNotes,
      activeNoteId: meta ? noteId : null,
      readNote,
      onNoteSaved: applySavedNoteMeta,
      getNoteLinkRefs,
    });

  const linkEditor = useLinkEditor(editor);

  // Wait for a confirmed save before the native webview can disappear.
  useEffect(() => {
    const win = getCurrentWindow();
    const subscription = win.onCloseRequested(event => {
      event.preventDefault();
      void closeEditor().then(() => win.destroy()).catch(() => {});
    });
    return () => { void subscription.then(unlisten => unlisten()); };
  },[closeEditor]);

  const togglePin = useCallback(async () => {
    const next = !pinned;
    try {
      await getCurrentWindow().setAlwaysOnTop(next);
      setPinned(next);
    } catch (e) {
      console.error("[note-window] failed to toggle pin:", e);
    }
  }, [pinned]);

  const existingNoteIds = useMemo(
    () => new Set(notes.map((n) => n.id)),
    [notes],
  );

  const linkContextValue = useMemo(
    () => ({
      notes,
      existingNoteIds,
      activeNoteId: noteId,
      // Clicking a [[link]] in a detached window opens the target in the
      // MAIN window (spec §5) — unless the target is itself detached, which
      // the main-window bridge resolves by focusing that window.
      onOpenNote: openNoteInMain,
      onRequestRecreate: (
        attrs: { id: string; title: string },
        onResolved: (newId: string) => void,
      ) => {
        setBrokenDialog({ title: attrs.title, onResolved });
      },
    }),
    [notes, existingNoteIds, noteId, openNoteInMain],
  );

  const handleRecreateConfirm = useCallback(async () => {
    if (!brokenDialog) return;
    const { title, onResolved } = brokenDialog;
    setBrokenDialog(null);
    try {
      const session = await getQueueSession();
      const created = await createNoteSynced(null);
      const safeTitle = title
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;");
      const seeded = await invoke<NoteMeta>("update_note", {
        id: created.id,
        content: `<h1>${safeTitle}</h1><p></p>`,
        title: title || created.title,
        session,
      });
      await emit("note-detached-updated", {
        ...session,
        id: seeded.id,
        title: seeded.title,
        updatedAt: seeded.updatedAt,
      });
      onResolved(created.id);
      await flushSave();
      openNoteInMain(created.id);
    } catch (e) {
      console.error("[note-window] failed to recreate linked note:", e);
    }
  }, [brokenDialog, flushSave, openNoteInMain]);

  const activeFolder = meta?.folderId
    ? folders.find((f) => f.id === meta.folderId) ?? null
    : null;

  if (loadFailed) {
    return (
      <div
        className="vt-app notes-shell flex items-center justify-center h-screen text-sm"
        style={{ background: "var(--vt-bg)", color: "var(--vt-fg-3)" }}
      >
        {t("notes.detach.loadError")}
      </div>
    );
  }

  return (
    <NoteLinkProvider value={linkContextValue}>
      <div
        className="vt-app notes-shell flex flex-col h-screen overflow-hidden"
        style={{ background: "var(--vt-bg)" }}
      >
        <div
          className="flex items-center justify-end gap-1 px-2 py-1 shrink-0 select-none"
          style={{
            borderBottom: "1px solid var(--vt-border)",
            background: "var(--vt-panel)",
          }}
        >
          <button
            type="button"
            className="footer-action"
            style={pinned ? { color: "var(--vt-accent)" } : undefined}
            onClick={() => void togglePin()}
            title={pinned ? t("notes.detach.unpin") : t("notes.detach.pin")}
            aria-label={pinned ? t("notes.detach.unpin") : t("notes.detach.pin")}
          >
            {pinned ? (
              <PinOff className="w-3.5 h-3.5" />
            ) : (
              <Pin className="w-3.5 h-3.5" />
            )}
          </button>
          <button
            type="button"
            className="footer-action"
            onClick={() => {
              // Flush the pending debounced save before the main window
              // closes this window and re-reads the note from disk.
              void flushSave().then(requestReattach).catch(() => {});
            }}
            title={t("notes.detach.reattach")}
            aria-label={t("notes.detach.reattach")}
          >
            <ArrowLeftToLine className="w-3.5 h-3.5" />
          </button>
        </div>

        {saveError && <div className="notes-error-banner" role="alert">{saveError}</div>}

        <NotesEditorContent
          editor={editor}
          hasActiveNote={meta !== null}
          isLoadingContent={isLoadingContent}
          loadedNoteId={loadedNoteId}
          activeNote={meta}
          activeFolder={activeFolder}
          linkEditor={linkEditor}
          onToggleLocalOnly={requestToggleLocalOnly}
          showShare={false}
        />

        <NotesEditorFooter
          editor={editor}
          hasActiveNote={meta !== null}
          loadedNoteId={loadedNoteId}
          activeNoteId={meta ? noteId : null}
          showAiAction={false}
          onRequestDelete={() => setConfirmDeleteOpen(true)}
        />

        <ConfirmDeleteDialog
          open={confirmDeleteOpen}
          title={t("notes.editor.deleteConfirmTitle")}
          description={t("notes.editor.deleteConfirmDesc")}
          onOpenChange={setConfirmDeleteOpen}
          onConfirm={() => {
            setConfirmDeleteOpen(false);
            void flushSave().then(requestDelete).catch(() => {});
          }}
        />

        <BrokenNoteLinkDialog
          open={brokenDialog !== null}
          title={brokenDialog?.title ?? ""}
          onOpenChange={(open) => {
            if (!open) setBrokenDialog(null);
          }}
          onConfirm={() => void handleRecreateConfirm()}
        />

        <Toaster position="bottom-right" theme={theme} />
      </div>
    </NoteLinkProvider>
  );
}
