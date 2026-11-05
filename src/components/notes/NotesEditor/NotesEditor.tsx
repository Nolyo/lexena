import { useCallback, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { type NoteData, type NoteMeta } from "@/hooks/useNotes";
import { type FolderMeta } from "@/hooks/useFolders";
import { useNotesEditorInstance } from "@/hooks/useNotesEditorInstance";
import { useAiAssistant } from "@/hooks/useAiAssistant";
import { useLinkEditor } from "@/hooks/useLinkEditor";
import { useCloud } from "@/hooks/useCloud";
import { ConfirmDeleteDialog } from "../ConfirmDeleteDialog";
import { NotesEditorTitleBar } from "./NotesEditorTitleBar";
import { NotesEditorContent } from "./NotesEditorContent";
import { NotesEditorFooter } from "./NotesEditorFooter";
import { NoteLinkProvider } from "./NoteLinkContext";
import { BrokenNoteLinkDialog } from "./BrokenNoteLinkDialog";
import { Backlinks } from "./Backlinks";

interface NotesEditorProps {
  notes: NoteMeta[];
  openNotes: NoteMeta[];
  activeNoteId: string | null;
  folders: FolderMeta[];
  onActivateNote: (id: string) => void;
  onOpenNoteInTab: (id: string) => void;
  onCloseNote: (id: string) => void;
  onDeleteNote: (id: string) => void | Promise<void>;
  onNoteSaved: (meta: NoteMeta) => void;
  onToggleLocalOnly: (id: string) => void;
  onCreateNote: () => void;
  /** Create a new note seeded with the given title. Does NOT open a tab —
   *  the editor handles that after flushing the pending save on the source note. */
  onRecreateLinkedNote: (title: string) => Promise<string>;
  onMoveNote: (noteId: string, folderId: string | null) => Promise<void>;
  onCreateFolder: (name: string, icon?: string | null) => Promise<FolderMeta>;
  readNote: (id: string) => Promise<NoteData>;
  onDetachNote: (id: string) => void;
  onDetachNoteAtCursor: (id: string) => void;
}

/**
 * Docked notes editor. Fills the entire main content area.
 * Composes three hooks (TipTap editor, AI assistant, link editor) with four
 * subcomponents (title bar, content, bubble menu, footer). The orchestrator
 * only handles delete confirmation and the "close empty note" policy.
 */
export function NotesEditor({
  notes,
  openNotes,
  activeNoteId,
  folders,
  onActivateNote,
  onOpenNoteInTab,
  onCloseNote,
  onDeleteNote,
  onNoteSaved,
  onToggleLocalOnly,
  onCreateNote,
  onRecreateLinkedNote,
  onMoveNote,
  onCreateFolder,
  readNote,
  onDetachNote,
  onDetachNoteAtCursor,
}: NotesEditorProps) {
  const { t } = useTranslation();

  // Ref read by the @-suggestion popup — always exposes the latest data
  // without rebuilding the TipTap editor.
  const linkRefsRef = useRef({ notes, activeNoteId });
  linkRefsRef.current = { notes, activeNoteId };
  const getNoteLinkRefs = useRef(() => linkRefsRef.current).current;

  // Bumped on every debounced save so the backlinks panel re-queries.
  const [backlinksRefresh, setBacklinksRefresh] = useState(0);
  const bumpBacklinks = useCallback(
    () => setBacklinksRefresh((n) => n + 1),
    [],
  );

  const { editor, isLoadingContent, loadedNoteId, flushSave, closeEditor, saveError } = useNotesEditorInstance({
    openNotes,
    activeNoteId,
    readNote,
    onNoteSaved,
    getNoteLinkRefs,
    onContentSaved: bumpBacklinks,
  });

  const ai = useAiAssistant(editor);
  const linkEditor = useLinkEditor(editor);
  const { isCloudEligible } = useCloud();

  const [confirmDeleteOpen, setConfirmDeleteOpen] = useState(false);

  // Broken-link recreation flow.
  const [brokenDialog, setBrokenDialog] = useState<{
    title: string;
    onResolved: (newId: string) => void;
  } | null>(null);

  const handleRecreateConfirm = useCallback(async () => {
    if (!brokenDialog) return;
    const { title, onResolved } = brokenDialog;
    setBrokenDialog(null);
    try {
      const newId = await onRecreateLinkedNote(title);
      onResolved(newId);
      await flushSave();
      onOpenNoteInTab(newId);
    } catch (err) {
      console.error("Failed to recreate linked note:", err);
    }
  }, [brokenDialog, onRecreateLinkedNote, flushSave, onOpenNoteInTab]);

  const hasActiveNote = openNotes.some((n) => n.id === activeNoteId);
  const activeNote =
    openNotes.find((n) => n.id === activeNoteId) ?? null;
  const activeFolder = activeNote?.folderId
    ? folders.find((f) => f.id === activeNote.folderId) ?? null
    : null;

  const existingNoteIds = useMemo(
    () => new Set(notes.map((n) => n.id)),
    [notes],
  );

  const linkContextValue = useMemo(
    () => ({
      notes,
      existingNoteIds,
      activeNoteId,
      // Clicking a [[mention]] must open the target as a tab if it isn't
      // already open, then activate it. `onActivateNote` only flips the
      // active id without touching the tab list, which left the editor on
      // the empty-state placeholder. `onOpenNoteInTab` does both.
      onOpenNote: (id:string) => { void flushSave().then(() => onOpenNoteInTab(id)).catch(() => {}); },
      onRequestRecreate: (
        attrs: { id: string; title: string },
        onResolved: (newId: string) => void,
      ) => {
        setBrokenDialog({ title: attrs.title, onResolved });
      },
    }),
    [notes, existingNoteIds, activeNoteId, onOpenNoteInTab, flushSave],
  );

  const isActiveNoteEmpty = (id: string): boolean => {
    if (id !== activeNoteId) return false;
    if (!editor) return false;
    return editor.getText().trim() === "";
  };

  const handleTabClose = async (id: string) => {
    if (isActiveNoteEmpty(id)) {
      try { await flushSave(); await onDeleteNote(id); } catch { return; }
    } else {
      try { if (id === activeNoteId) await closeEditor(); } catch { return; }
      onCloseNote(id);
    }
  };

  const handleConfirmDelete = async () => {
    try { await flushSave(); if (activeNoteId) await onDeleteNote(activeNoteId); } catch { return; }
    setConfirmDeleteOpen(false);
  };

  return (
    <NoteLinkProvider value={linkContextValue}>
      <div
        className="vt-app notes-shell flex flex-col h-full overflow-hidden"
        style={{ background: "var(--vt-bg)" }}
      >
        <NotesEditorTitleBar
          openNotes={openNotes}
          activeNoteId={activeNoteId}
          loadedNoteId={loadedNoteId}
          folders={folders}
          editor={editor}
          onActivateNote={(id) => { void flushSave().then(() => onActivateNote(id)).catch(() => {}); }}
          onTabClose={(id) => { void handleTabClose(id); }}
          onCreateNote={() => { void flushSave().then(onCreateNote).catch(() => {}); }}
          onMoveNote={onMoveNote}
          onCreateFolder={onCreateFolder}
          onDetachNote={(id) => {
            // Flush the 500 ms debounced save before ownership moves to the
            // detached window — otherwise the timer can fire after the tab
            // switch and write the NEXT note's HTML under this note's id.
            void flushSave().then(() => onDetachNote(id)).catch(() => {});
          }}
          onDetachNoteAtCursor={(id) => {
            // Same flush-before-transfer invariant as onDetachNote above —
            // a drag-out is another ownership transfer to a detached window.
            void flushSave().then(() => onDetachNoteAtCursor(id)).catch(() => {});
          }}
        />

        {saveError && <div className="notes-error-banner" role="alert">{saveError}</div>}

        <NotesEditorContent
          editor={editor}
          hasActiveNote={hasActiveNote}
          isLoadingContent={isLoadingContent}
          loadedNoteId={loadedNoteId}
          activeNote={activeNote}
          activeFolder={activeFolder}
          ai={ai}
          linkEditor={linkEditor}
          onToggleLocalOnly={onToggleLocalOnly}
        />

        {hasActiveNote && (
          <Backlinks
            noteId={activeNoteId}
            refreshKey={backlinksRefresh}
            onOpen={onActivateNote}
          />
        )}

        {ai.state === "error" && ai.error && (
          <div className="notes-error-banner" onClick={ai.dismiss}>
            {ai.error}
          </div>
        )}

        <NotesEditorFooter
          editor={editor}
          hasActiveNote={hasActiveNote}
          loadedNoteId={loadedNoteId}
          activeNoteId={activeNoteId}
          isAiLoading={ai.state === "loading"}
          isAiEligible={isCloudEligible}
          onAiAction={ai.processSelection}
          onRequestDelete={() => setConfirmDeleteOpen(true)}
        />

        <ConfirmDeleteDialog
          open={confirmDeleteOpen}
          title={t("notes.editor.deleteConfirmTitle")}
          description={t("notes.editor.deleteConfirmDesc")}
          onOpenChange={setConfirmDeleteOpen}
          onConfirm={handleConfirmDelete}
        />

        <BrokenNoteLinkDialog
          open={brokenDialog !== null}
          title={brokenDialog?.title ?? ""}
          onOpenChange={(open) => {
            if (!open) setBrokenDialog(null);
          }}
          onConfirm={handleRecreateConfirm}
        />
      </div>
    </NoteLinkProvider>
  );
}
