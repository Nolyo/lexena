import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { EditorOwnership, type EditorContext, type EditorRead } from "@/lib/notes/editor-ownership";
import { useEditor, ReactNodeViewRenderer } from "@tiptap/react";
import { CodeBlockLanguageSelect } from "@/components/notes/NotesEditor/CodeBlockLanguageSelect";
import StarterKit from "@tiptap/starter-kit";
import CodeBlockLowlight from "@tiptap/extension-code-block-lowlight";
import { createLowlight, common } from "lowlight";
import { Table } from "@tiptap/extension-table";
import { TableRow } from "@tiptap/extension-table-row";
import { TableHeader } from "@tiptap/extension-table-header";
import { TableCell } from "@tiptap/extension-table-cell";
import Image from "@tiptap/extension-image";
import Placeholder from "@tiptap/extension-placeholder";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import { TextStyle } from "@tiptap/extension-text-style";
import { Color } from "@tiptap/extension-color";
import { Highlight } from "@tiptap/extension-highlight";
import i18n from "@/i18n";
import { type NoteData, type NoteMeta, deriveTitle } from "@/hooks/useNotes";
import { NoteLink } from "@/components/notes/NotesEditor/NoteLinkExtension";
import { buildNoteLinkSuggestion } from "@/components/notes/NotesEditor/NoteLinkSuggestion";
import { SlashCommand } from "@/components/notes/NotesEditor/SlashCommand/SlashCommandExtension";

const lowlight = createLowlight(common);

/** Layout the editor falls back to when a note has no persisted content.
 *  Surfaces a visible "title slot" so new users grasp the first-line-is-title
 *  convention without reading docs. Empty H1 + empty P produce no text nodes,
 *  so `editor.getText()` still returns `""` and the "delete-on-close-empty"
 *  policy in NotesEditor keeps working unchanged. */
const EMPTY_NOTE_SEED = "<h1></h1><p></p>";

/** Content strings that should be treated as "empty" and replaced by the
 *  seed. Covers the backend's literal empty file, whitespace, and the
 *  single-paragraph shape TipTap serialises for an empty doc. */
function isBlankContent(raw: string): boolean {
  if (!raw) return true;
  const stripped = raw.replace(/\s+/g, "");
  return stripped === "" || stripped === "<p></p>";
}

interface UseNotesEditorInstanceOptions {
  openNotes: NoteMeta[];
  activeNoteId: string | null;
  readNote: (id: string) => Promise<NoteData>;
  onNoteSaved: (meta: NoteMeta) => void;
  /**
   * Returns the *current* notes list + active note id for the @-suggestion
   * popup. Must stay identity-stable across renders; internally we read from
   * a ref so it's always fresh without rebuilding the editor.
   */
  getNoteLinkRefs: () => { notes: NoteMeta[]; activeNoteId: string | null };
  /** Bumped every time the active note's content is saved — used by the
   *  parent to refresh the backlinks panel when the user adds/removes links. */
  onContentSaved?: () => void;
}

/**
 * Owns the TipTap editor instance used by the notes modal:
 *
 * - Configures StarterKit (with inline-safe Link), Image, Placeholder, TaskList
 * - Handles image paste/drop by reading files as data URIs
 * - Opens Ctrl+clicked links externally via `@tauri-apps/plugin-opener`
 * - Registers a profile/session/revision-bound owner before loading content
 * - Sets the dirty barrier synchronously and debounces revision-checked saves
 * - Retains drafts and blocks ownership transfer when persistence fails
 */
export function useNotesEditorInstance({
  openNotes,
  activeNoteId,
  onNoteSaved,
  getNoteLinkRefs,
  onContentSaved,
}: UseNotesEditorInstanceOptions) {
  const [isLoadingContent, setIsLoadingContent] = useState(false);
  // `loadedNoteId` is exposed to renderers so they can gate any UI that
  // derives from the editor's current document (live tab title, word count)
  // until TipTap has actually ingested the active note's content. Without
  // this, clicking a tab shows the previous note's title/word-count for one
  // paint because `activeNoteId` flips before `readNote()` resolves.
  const [loadedNoteId, setLoadedNoteId] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const ownershipRef = useRef<EditorOwnership | null>(null);
  const transitionRef = useRef<Promise<void>>(Promise.resolve());
  const sessionRef = useRef<Promise<{profile_id:string;session_generation:number}> | null>(null);
  const callbacksRef = useRef({onNoteSaved,onContentSaved});
  callbacksRef.current = {onNoteSaved,onContentSaved};
  const saveTimerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  // `handleImageFile` needs the editor instance, but the editor's
  // editorProps closures below reference it before it is defined. Using a
  // ref lets the closures resolve the latest implementation at event time,
  // after we've bound the real function in the assignment that follows
  // `useEditor(...)`.
  const handleImageFileRef = useRef<(file: File) => void>(() => {});

  const editor = useEditor({
    extensions: [
      StarterKit.configure({
        codeBlock: false,
        // Link is bundled in StarterKit v3; keep openOnClick: false so our
        // existing Ctrl+clic handler (see handleDOMEvents.click) remains the
        // only place that opens links externally via Tauri plugin-opener.
        link: {
          openOnClick: false,
          autolink: true,
          HTMLAttributes: {
            rel: "noopener noreferrer nofollow",
          },
        },
      }),
      CodeBlockLowlight.extend({
        addNodeView() {
          return ReactNodeViewRenderer(CodeBlockLanguageSelect);
        },
      }).configure({
        lowlight,
        defaultLanguage: "plaintext",
        HTMLAttributes: {
          class: "vt-code-block",
        },
      }),
      Table.configure({
        resizable: true,
        HTMLAttributes: { class: "vt-table" },
      }),
      TableRow,
      TableHeader,
      TableCell,
      Image.configure({
        inline: true,
        allowBase64: true,
      }),
      Placeholder.configure({
        // Show placeholders on every empty node (not just the focused one)
        // so the seeded <h1> and <p> both display hints before the user
        // touches the doc.
        showOnlyCurrent: false,
        placeholder: ({ node, editor, pos }) => {
          const { doc } = editor.state;
          // Node at position 0 is the first child of the doc.
          if (pos === 0 && node.type.name === "heading" && node.attrs.level === 1) {
            return i18n.t("notes.editor.titlePlaceholder", {
              defaultValue: "Titre de la note",
            });
          }
          // The slot directly after an opening H1 gets the body hint, but
          // only while the note is still in its "blank" shape (H1 + one
          // empty paragraph). Once the user adds content further down, the
          // hint is no longer relevant and would otherwise stick forever
          // on the empty line between the title and the first real block.
          const first = doc.firstChild;
          if (
            first &&
            first.type.name === "heading" &&
            pos === first.nodeSize &&
            node.type.name === "paragraph" &&
            doc.childCount === 2
          ) {
            return i18n.t("notes.editor.bodyPlaceholder", {
              defaultValue: "Commencez à écrire…",
            });
          }
          return "";
        },
      }),
      TaskList,
      TaskItem.configure({
        nested: true,
      }),
      TextStyle,
      Color,
      Highlight.configure({ multicolor: true }),
      NoteLink.configure({
        HTMLAttributes: {},
        suggestion: buildNoteLinkSuggestion(getNoteLinkRefs),
      }),
      SlashCommand,
    ],
    editorProps: {
      attributes: {
        class:
          "prose prose-sm dark:prose-invert max-w-none w-full h-full p-4 focus:outline-none text-foreground text-sm leading-relaxed",
      },
      handleDOMEvents: {
        click: (_view, event) => {
          const target = event.target as HTMLElement;
          const link = target.closest("a");
          if (!link) return false;

          event.preventDefault();

          if (event.ctrlKey || event.metaKey) {
            const href = link.getAttribute("href");
            if (href) {
              import("@tauri-apps/plugin-opener").then(({ openUrl }) =>
                openUrl(href),
              );
            }
          }
          return true;
        },
      },
      handlePaste: (_view, event) => {
        const items = event.clipboardData?.items;
        if (!items) return false;

        for (const item of items) {
          if (item.type.startsWith("image/")) {
            event.preventDefault();
            const file = item.getAsFile();
            if (file) handleImageFileRef.current(file);
            return true;
          }
        }
        return false;
      },
      handleDrop: (_view, event) => {
        const files = event.dataTransfer?.files;
        if (!files || files.length === 0) return false;

        for (const file of files) {
          if (file.type.startsWith("image/")) {
            event.preventDefault();
            handleImageFileRef.current(file);
            return true;
          }
        }
        return false;
      },
    },
    onUpdate: ({ editor }) => {
      clearTimeout(saveTimerRef.current);
      const owner = ownershipRef.current;
      if (!owner) return;
      // The renderer barrier is synchronous; IPC is dispatched before debounce.
      owner.change(editor.getHTML());
      saveTimerRef.current = setTimeout(() => {
        saveTimerRef.current = undefined;
        void owner.flush().catch(() => {});
      }, 500);
    },
  });

  // Bind the real handler into the ref. This runs on every render, so the
  // ref always points at the latest closure capturing the current editor.
  handleImageFileRef.current = (file: File) => {
    if (!editor) return;

    const reader = new FileReader();
    reader.onload = () => {
      const dataUri = reader.result;
      if (typeof dataUri === "string") {
        editor.chain().focus().setImage({ src: dataUri }).run();
      }
    };
    reader.onerror = () => {
      console.error("Failed to read image file");
    };
    reader.readAsDataURL(file);
  };

  // Every delayed operation carries the session captured by this renderer.
  // Switching tabs waits for a confirmed save; a failure keeps the visible draft.
  useEffect(() => {
    if (!editor) return;
    let cancelled = false;
    sessionRef.current ??= invoke("get_profile_session");
    const sessionPromise = sessionRef.current;
    const transition = transitionRef.current.then(async () => {
      if (cancelled) return;
      const previous = ownershipRef.current;
      if (previous?.context.note_id === activeNoteId) {
        // A canceled switch may have failed to close this owner. Its finally
        // cannot clear loading; expose the retained draft without clearing
        // the save error or re-enabling an owner with a persistence failure.
        setIsLoadingContent(false);
        return;
      }
      clearTimeout(saveTimerRef.current);
      editor.setEditable(false);
      setIsLoadingContent(true);
      try {
        if (previous) {
          await previous.close();
          if (ownershipRef.current === previous) ownershipRef.current = null;
        }
        if (cancelled) return;
        if (!activeNoteId) {
          setLoadedNoteId(null);
          return;
        }
        const session = await sessionPromise;
        if (cancelled) return;
        const data = await invoke<EditorRead>("register_note_editor", {
          noteId: activeNoteId, editorId: crypto.randomUUID(), session,
        });
        if (cancelled) {
          await invoke("end_note_edit", {context:data.context});
          return;
        }
        const draftKey = (context:EditorContext) => `lexena:note-draft:${context.profile_id}:${context.note_id}`;
        const load = (content:string) => editor.commands.setContent(isBlankContent(content) ? EMPTY_NOTE_SEED : content,{emitUpdate:false});
        const owner = new EditorOwnership(data.context,data.note.content,{
          begin:context => invoke("begin_note_edit",{context}),
          save:(context,content) => invoke<EditorRead>("save_editor_note",{context,content,title:deriveTitle(content),operationId:crypto.randomUUID()}),
          ack:(context,prepareId,dirty) => invoke("ack_editor_prepare",{context,prepareId,dirty}),
          read:context => invoke<EditorRead>("read_editor_note",{context}),
          end:context => invoke("end_note_edit",{context}),
        },{
          setEditable:value => editor.setEditable(value),
          load,
          conflict:error => {
            console.error("Note draft retained:",error);
            setSaveError(i18n.t("notes.editor.saveConflict",{defaultValue:"Votre brouillon est conservé. La note a changé ou la sauvegarde a échoué. Copiez votre texte avant de fermer cet éditeur."}));
          },
          persist:(context,content) => {
            // A crash or rejected close must preserve the last keystroke.
            try {
              if (content === null) localStorage.removeItem(draftKey(context));
              else localStorage.setItem(draftKey(context),JSON.stringify({content,revision:context.loaded_revision}));
            } catch (error) { console.error("Draft recovery storage unavailable",error); }
          },
          saved:saved => {
            callbacksRef.current.onNoteSaved(saved.note.meta as unknown as NoteMeta);
            callbacksRef.current.onContentSaved?.();
          },
        });
        ownershipRef.current = owner;
        setSaveError(null);
        const draft = localStorage.getItem(draftKey(data.context));
        if (draft) {
          try {
            const stored:unknown = JSON.parse(draft);
            if (stored && typeof stored === "object" && "content" in stored && "revision" in stored && typeof stored.content === "string" && typeof stored.revision === "string") {
              owner.restoreDraft(stored.content,stored.revision);
            }
          } catch (error) { console.error("Failed to recover note draft",error); }
        }
        load(owner.content);
        setLoadedNoteId(activeNoteId);
        editor.setEditable(!owner.conflicted);
        if (!owner.conflicted && isBlankContent(owner.content)) editor.commands.focus("start");
      } catch (error) {
        console.error("Failed to switch note editor:",error);
        setSaveError(i18n.t("notes.editor.saveConflict",{defaultValue:"Votre brouillon est conservé. La sauvegarde a échoué ; copiez votre texte avant de fermer cet éditeur."}));
        // The previous editor and draft stay in place when flush/close fails.
      } finally { if (!cancelled) setIsLoadingContent(false); }
    });
    transitionRef.current = transition.catch(() => {});
    return () => { cancelled = true; };
  }, [activeNoteId,editor]);

  useEffect(() => {
    const matches = (context:EditorContext) => {
      const owner = ownershipRef.current;
      return owner && owner.context.editor_id === context.editor_id && owner.context.profile_id === context.profile_id && owner.context.session_generation === context.session_generation ? owner : null;
    };
    const subscriptions = [
      listen<{context:EditorContext;prepare_id:string}>("note-editor-prepare",event => {
        void matches(event.payload.context)?.prepare(event.payload.prepare_id).catch(() => {});
      }),
      listen<{context:EditorContext;prepare_id:string;committed:boolean}>("note-editor-resume",event => {
        void matches(event.payload.context)?.resume(event.payload.prepare_id,event.payload.committed);
      }),
      listen<{profile_id:string;note_id:string}>("note-service-changed",event => {
        const owner = ownershipRef.current;
        if (owner && owner.context.profile_id === event.payload.profile_id && owner.context.note_id === event.payload.note_id) void owner.refresh();
      }),
    ];
    return () => { for (const subscription of subscriptions) void subscription.then(unlisten => unlisten()); };
  },[]);

  useEffect(() => () => {
    clearTimeout(saveTimerRef.current);
    const owner = ownershipRef.current;
    if (owner) void owner.close().catch(error => console.error("Unsaved note draft retained",error));
  },[]);

  const flushSave = useCallback(async () => {
    clearTimeout(saveTimerRef.current);
    saveTimerRef.current = undefined;
    await ownershipRef.current?.flush();
  },[]);
  const closeEditor = useCallback(async () => {
    clearTimeout(saveTimerRef.current);
    const owner = ownershipRef.current;
    await owner?.close();
    if (ownershipRef.current === owner) ownershipRef.current = null;
  },[]);

  // openNotes remains part of the shared main/detached hook contract.
  void openNotes;
  return {editor,isLoadingContent,loadedNoteId,flushSave,closeEditor,saveError};
}
