import { invoke } from "@tauri-apps/api/core";
import type { CloudUserFolderRow, LocalFolderMeta } from "./types";
import { mapFolderFromCloud } from "./mapping";
import { requestOutboxDrain } from "./local-outbox";
import { getQueueSession } from "./queue";
import type { RemoteApplyResult } from "./notes-store";
export async function listFolders(): Promise<LocalFolderMeta[]> { return invoke("list_folders"); }
async function mutate<T>(command: string, args: Record<string, unknown>): Promise<T> { const result = await invoke<T>(command, { ...args, session: await getQueueSession() }); requestOutboxDrain(); return result; }
export function createFolderSynced(name: string, icon?: string | null): Promise<LocalFolderMeta> { return mutate("create_folder", { name, icon: icon ?? null }); }
export function renameFolderSynced(id: string, name: string, icon?: string | null): Promise<LocalFolderMeta> { return mutate("rename_folder", { id, name, icon: icon ?? null }); }
export function deleteFolderSynced(id: string): Promise<void> { return mutate("delete_folder", { id }); }
export async function pushFolderUpsert(_folder: LocalFolderMeta): Promise<void> { requestOutboxDrain(); }
export async function applyRemoteFolder(row: CloudUserFolderRow): Promise<RemoteApplyResult> {
  return invoke("apply_sync_remote_folder", { session: await getQueueSession(), folder: mapFolderFromCloud(row) });
}
