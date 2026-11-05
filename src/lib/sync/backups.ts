import { invoke } from "@tauri-apps/api/core";
import { Store } from "@tauri-apps/plugin-store";
import { getQueueSession } from "./queue";

export interface BackupMeta {
  filename: string;
  created_at: string;
  size_bytes: number;
}

/** Full note meta shape as serialized by the Rust backend (camelCase).
 *
 * Intentionally richer than `useNotes.NoteMeta` because backups must preserve
 * tombstones (`deletedAt`) verbatim — `useNotes` only exposes the visible
 * subset. Keep these fields in sync with `src-tauri/src/notes.rs::NoteMeta`. */
export interface BackupNoteMeta {
  id: string;
  title: string;
  createdAt: string;
  updatedAt: string;
  favorite: boolean;
  folderId?: string | null;
  order: number;
  /** ISO RFC3339 timestamp when the note was soft-deleted, or absent/null. */
  deletedAt?: string | null;
  /** True = never synced to the cloud. Round-trips verbatim through restore. */
  localOnly?: boolean;
}

/** Full folder meta shape as serialized by the Rust backend (camelCase).
 *
 * Intentionally richer than `useFolders.FolderMeta`: backups must round-trip
 * `updatedAt` (LWW) and `deletedAt` (tombstone). Keep in sync with
 * `src-tauri/src/folders.rs::FolderMeta`. */
export interface BackupFolderMeta {
  id: string;
  name: string;
  icon?: string;
  createdAt: string;
  updatedAt: string;
  order: number;
  deletedAt?: string | null;
}

export interface NoteWithContent {
  meta: BackupNoteMeta;
  content: string;
}

/** v1: tauri_stores only.
 *  v2: + notes + folders (sub-epic 03 sync-notes, Task 11). */
export interface BackupPayloadV1 {
  version: 1;
  created_at: string;
  tauri_stores: Record<string, unknown>;
}

export interface BackupPayloadV2 {
  version: 2;
  created_at: string;
  tauri_stores: Record<string, unknown>;
  notes: NoteWithContent[];
  folders: BackupFolderMeta[];
}

export type BackupPayload = BackupPayloadV1 | BackupPayloadV2;

const STORES_TO_SNAPSHOT = [
  "settings.json",
  "sync-snippets.json",
  "sync-dictionary.json",
];

/** Capture les Tauri Stores listés + sérialise en JSON pour écriture locale. */
async function snapshotStores(): Promise<Record<string, unknown>> {
  const snapshot: Record<string, unknown> = {};
  for (const file of STORES_TO_SNAPSHOT) {
    try {
      const s = await Store.load(file);
      const entries = await s.entries();
      snapshot[file] = Object.fromEntries(entries);
    } catch (e) {
      console.warn(`[backup] cannot snapshot ${file}:`, e);
      snapshot[file] = null;
    }
  }
  return snapshot;
}

/** Capture every note (with full HTML content) for backup.
 *
 * Strategy:
 *  - `list_notes` returns active notes only (Rust filters tombstones). That is
 *    intentional: we don't want to leak soft-deleted notes into a fresh
 *    machine after restore. If a tombstone needs to survive, sync handles it
 *    via its own LWW logic — backups are a local safety net for the visible
 *    state.
 *  - Per-note read errors are logged and skipped so a single corrupt note
 *    doesn't abort the whole backup. */
async function snapshotNotes(): Promise<NoteWithContent[]> {
  let metas: BackupNoteMeta[];
  try {
    metas = await invoke<BackupNoteMeta[]>("list_notes");
  } catch (e) {
    console.warn("[backup] cannot list notes:", e);
    return [];
  }

  const out: NoteWithContent[] = [];
  for (const meta of metas) {
    try {
      const data = await invoke<{ meta: BackupNoteMeta; content: string }>(
        "read_note",
        { id: meta.id },
      );
      out.push({ meta: data.meta, content: data.content });
    } catch (e) {
      console.warn(`[backup] cannot read note ${meta.id}:`, e);
    }
  }
  return out;
}

/** Capture all active folders (Rust `list_folders` filters tombstones). */
async function snapshotFolders(): Promise<BackupFolderMeta[]> {
  try {
    return await invoke<BackupFolderMeta[]>("list_folders");
  } catch (e) {
    console.warn("[backup] cannot list folders:", e);
    return [];
  }
}

/** Crée un backup local. Retourne le filename (ex. `pre-sync_2026-04-24_143012.json`). */
export async function createLocalBackup(): Promise<string> {
  const payload: BackupPayloadV2 = {
    version: 2,
    created_at: new Date().toISOString(),
    tauri_stores: await snapshotStores(),
    notes: await snapshotNotes(),
    folders: await snapshotFolders(),
  };
  return invoke<string>("write_local_backup", { payloadJson: JSON.stringify(payload) });
}

export async function listLocalBackups(): Promise<BackupMeta[]> {
  return invoke<BackupMeta[]>("list_local_backups");
}

export async function readLocalBackup(filename: string): Promise<BackupPayload> {
  const raw = await invoke<string>("read_local_backup", { filename });
  return JSON.parse(raw) as BackupPayload;
}

export async function deleteLocalBackup(filename: string): Promise<void> {
  await invoke("delete_local_backup", { filename });
}

/** Restaure : écrase chaque Tauri Store + (v2) ré-importe notes & dossiers.
 *
 * Stratégie pour limiter les casses en cas d'échec partiel :
 *  1. Tauri Stores : on set d'abord toutes les clés du backup (si crash après set / avant delete,
 *     on a un superset stale plutôt qu'un store vidé). Puis on supprime les clés absentes.
 *  2. Folders : restaurés avant les notes qui les référencent.
 *  3. Notes : un try/catch par note pour qu'une note corrompue ne bloque pas les autres. */
export async function restoreLocalBackup(filename: string): Promise<{
  restored: string[];
  failed: Array<{ file: string; error: string }>;
  notes_restored: number;
  folders_restored: number;
}> {
  const session = await getQueueSession();
  const payload = await readLocalBackup(filename);
  const restored: string[] = [];
  const failed: Array<{ file: string; error: string }> = [];

  // ── 1) Tauri Stores ────────────────────────────────────────────────────────
  for (const [file, content] of Object.entries(payload.tauri_stores)) {
    if (!content || typeof content !== "object") continue;
    try {
      const store = await Store.load(file);
      const backupKeys = new Set(Object.keys(content as Record<string, unknown>));

      // 1) Set all backup keys first (safe if save fails mid-loop: local is a superset, not a subset)
      for (const [k, v] of Object.entries(content as Record<string, unknown>)) {
        await store.set(k, v);
      }

      // 2) Remove local keys that weren't in the backup
      const currentKeys = await store.keys();
      for (const k of currentKeys) {
        if (!backupKeys.has(k)) {
          await store.delete(k);
        }
      }

      await store.save();
      restored.push(file);
    } catch (e) {
      failed.push({ file, error: e instanceof Error ? e.message : String(e) });
    }
  }

  // ── 2) Notes + folders (v2 only — v1 backups skip this gracefully) ────────
  let notes_restored = 0;
  let folders_restored = 0;

  // Backward compat: v1 payloads (or v2 with missing/non-array sections) skip.
  const notes = "notes" in payload && Array.isArray(payload.notes) ? payload.notes : null;
  const folders = "folders" in payload && Array.isArray(payload.folders) ? payload.folders : null;

  if (folders) {
    try {
      await invoke("import_folders_for_backup", { folders, source: "backup", session });
      folders_restored = folders.length;
    } catch (e) {
      failed.push({
        file: "folders.json",
        error: e instanceof Error ? e.message : String(e),
      });
    }
  }

  if (notes) {
    for (const note of notes) {
      try {
        await invoke("import_note_for_backup", {
          meta: note.meta,
          content: note.content,
          source: "backup",
          session,
        });
        notes_restored++;
      } catch (e) {
        failed.push({
          file: `note:${note.meta?.id ?? "unknown"}`,
          error: e instanceof Error ? e.message : String(e),
        });
      }
    }
  }

  return { restored, failed, notes_restored, folders_restored };
}
