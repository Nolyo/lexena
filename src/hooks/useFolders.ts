import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  createFolderSynced,
  renameFolderSynced,
  deleteFolderSynced,
  pushFolderUpsert,
} from '@/lib/sync/folders-store';
import type { LocalFolderMeta } from '@/lib/sync/types';
import { getQueueSession } from '@/lib/sync/queue';
import { createScopedMetadataRefresh, type MetadataChange, type MetadataSession } from '@/lib/notes/metadata-refresh';

export interface FolderMeta {
  id: string;
  name: string;
  /** Emoji icon; absent = default folder glyph. */
  icon?: string;
  createdAt: string;
  updatedAt?: string;
  order: number;
  deletedAt?: string;
}

export function useFolders() {
  const [folders, setFolders] = useState<FolderMeta[]>([]);
  const [isLoading, setIsLoading] = useState(true);

  const loadFolders = useCallback(async () => {
    try {
      setIsLoading(true);
      const result = await invoke<FolderMeta[]>('list_folders');
      setFolders(result);
    } catch (error) {
      console.error('Failed to load folders:', error);
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    loadFolders();
  }, [loadFolders]);

  useEffect(() => {
    let disposed = false;
    let refresh: ReturnType<typeof createScopedMetadataRefresh<FolderMeta>> | undefined;
    const subscription = invoke<MetadataSession>('get_profile_session').then(session => {
      if (disposed) return () => {};
      refresh = createScopedMetadataRefresh<FolderMeta>({
        session,
        currentSession: () => invoke<MetadataSession>('get_profile_session'),
        fetch: () => invoke<FolderMeta[]>('list_folders'),
        apply: (_ids, result) => setFolders(result),
        onError: error => console.warn('Folder metadata refresh failed', error),
      });
      return listen<Omit<MetadataChange, 'note_id'>>('folder-service-changed', event =>
        refresh?.enqueue({ ...event.payload, note_id: 'folders' }));
    });
    return () => { disposed = true; refresh?.dispose(); void subscription.then(unlisten => unlisten()).catch(() => {}); };
  }, []);

  const createFolder = async (name: string, icon?: string | null): Promise<FolderMeta> => {
    const meta = await createFolderSynced(name, icon);
    setFolders(prev => [...prev, meta].sort((a, b) => a.order - b.order));
    return meta;
  };

  const renameFolder = async (id: string, name: string, icon?: string | null): Promise<FolderMeta> => {
    const updated = await renameFolderSynced(id, name, icon);
    setFolders(prev => prev.map(f => f.id === id ? updated : f));
    return updated;
  };

  const deleteFolder = async (id: string): Promise<void> => {
    await deleteFolderSynced(id);
    setFolders(prev => prev.filter(f => f.id !== id));
  };

  const reorderFolders = async (ids: string[]): Promise<void> => {
    setFolders(prev => {
      const indexOf = new Map(ids.map((id, i) => [id, i]));
      return [...prev]
        .map(f => {
          const idx = indexOf.get(f.id);
          return idx !== undefined ? { ...f, order: idx } : f;
        })
        .sort((a, b) => a.order - b.order);
    });
    try {
      await invoke('reorder_folders', { ids, session: await getQueueSession() });
      // Re-read the persisted folders and enqueue an upsert per reordered folder so
      // the cloud picks up the new order via LWW (each folder's `updated_at` was
      // bumped server-side by reorder_folders).
      const persisted = await invoke<LocalFolderMeta[]>('list_folders');
      for (const folderId of ids) {
        const folder = persisted.find(f => f.id === folderId);
        if (!folder) continue;
        await pushFolderUpsert(folder);
      }
    } catch (error) {
      console.error('Failed to reorder folders:', error);
      await loadFolders();
    }
  };

  return {
    folders,
    isLoading,
    loadFolders,
    createFolder,
    renameFolder,
    deleteFolder,
    reorderFolders,
    reloadFolders: loadFolders,
  };
}
