// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import type { NoteMeta } from './useNotes';
import type { FolderMeta } from './useFolders';

type Handler = (event: { payload: any }) => void;
const native = vi.hoisted(() => ({
  invoke: vi.fn(), listeners: new Map<string, Handler>(), focus: undefined as Handler | undefined,
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn(async () => {}),
  listen: vi.fn(async (name: string, handler: Handler) => {
    native.listeners.set(name, handler);
    return () => { if (native.listeners.get(name) === handler) native.listeners.delete(name); };
  }),
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({
  setTitle: async () => {},
  onFocusChanged: async (handler: Handler) => { native.focus = handler; return () => { native.focus = undefined; }; },
}) }));
import { useDetachedNote } from './useDetachedNote';

const session = { profile_id: 'work', session_generation: 4 };
const note = (id: string, title: string): NoteMeta => ({ id, title, createdAt: 'now', updatedAt: 'now', order: 0, favorite: false });
const folder = (name: string): FolderMeta => ({ id: 'folder', name, createdAt: 'now', order: 0 });
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
async function flush() { await act(async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); }); }
beforeEach(() => {
  vi.useFakeTimers(); native.invoke.mockReset(); native.listeners.clear(); native.focus = undefined;
  native.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return session;
    if (command === 'list_notes') return [note('own', 'Before'), note('wiki', 'Wiki target')];
    if (command === 'list_folders') return [folder('Before folder')];
    throw new Error(`Unexpected native command: ${command}`);
  });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

// The actual hook/coalescer run; only the asynchronous native transport is replaced.
it.each(['updated', 'deleted'] as const)('keeps %s metadata when an older focus snapshot finishes', async kind => {
  const { result } = renderHook(() => useDetachedNote('own'));
  await flush();
  expect(result.current.meta?.title).toBe('Before');
  const oldFolders = deferred<FolderMeta[]>();
  native.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return session;
    if (command === 'list_notes') return [note('own', 'Before'), note('wiki', 'Wiki target')];
    if (command === 'list_folders') return oldFolders.promise;
    if (command === 'read_note_metadata_batch') return kind === 'updated' ? [note('own', 'Fresh')] : [];
    throw new Error(command);
  });
  await act(async () => native.focus?.({ payload: true }));
  await act(async () => {
    native.listeners.get('note-service-changed')!({ payload: { ...session, note_id: 'own' } });
    await vi.advanceTimersByTimeAsync(25);
  });
  expect(result.current.meta?.title ?? null).toBe(kind === 'updated' ? 'Fresh' : null);
  await act(async () => oldFolders.resolve([folder('Before folder')]));
  await flush();
  expect(result.current.meta?.title ?? null).toBe(kind === 'updated' ? 'Fresh' : null);
  expect(result.current.loadFailed).toBe(kind === 'deleted');
  expect(result.current.notes.find(n => n.id === 'wiki')?.title).toBe('Wiki target');
  expect(result.current.notes.some(n => n.id === 'own')).toBe(kind === 'updated');
});

it('merges untouched initial wiki targets while note events keep arriving and keeps newer folder metadata', async () => {
  const initialFolders = deferred<FolderMeta[]>();
  let folderReads = 0;
  native.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return session;
    if (command === 'list_notes') return [note('own', 'Before'), note('wiki', 'Wiki target')];
    if (command === 'list_folders') return ++folderReads === 1 ? initialFolders.promise : [folder('Fresh folder')];
    if (command === 'read_note_metadata_batch') return [note('own', 'Fresh')];
    throw new Error(command);
  });
  const { result } = renderHook(() => useDetachedNote('own'));
  await flush();
  await act(async () => {
    native.listeners.get('note-service-changed')!({ payload: { ...session, note_id: 'own' } });
    native.listeners.get('folder-service-changed')!({ payload: session });
    await vi.advanceTimersByTimeAsync(25);
  });
  expect(result.current.meta?.title).toBe('Fresh');
  expect(result.current.folders[0]?.name).toBe('Fresh folder');
  // Another autosave is queued as the initial full snapshot resolves.
  await act(async () => {
    native.listeners.get('note-service-changed')!({ payload: { ...session, note_id: 'own' } });
    initialFolders.resolve([folder('Before folder')]);
  });
  await flush();
  expect(result.current.meta?.title).toBe('Fresh');
  expect(result.current.folders[0]?.name).toBe('Fresh folder');
  expect(result.current.notes.find(n => n.id === 'wiki')?.title).toBe('Wiki target');
});

it('orders concurrent full snapshots and discards results after a profile A-B-A generation change', async () => {
  const { result } = renderHook(() => useDetachedNote('own'));
  await flush();
  const older = deferred<FolderMeta[]>();
  let reads = 0;
  let current = session;
  native.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return current;
    if (command === 'list_notes') return [note('own', ++reads === 1 ? 'Older' : 'Newest')];
    if (command === 'list_folders') return reads === 1 ? older.promise : [folder('Newest folder')];
    throw new Error(command);
  });
  await act(async () => native.focus?.({ payload: true }));
  await act(async () => native.focus?.({ payload: true }));
  await flush();
  expect(result.current.meta?.title).toBe('Newest');
  await act(async () => older.resolve([folder('Older folder')]));
  await flush();
  expect(result.current.meta?.title).toBe('Newest');
  current = { ...session, session_generation: 6 };
  await act(async () => native.focus?.({ payload: true }));
  await flush();
  expect(result.current.meta?.title).toBe('Newest');
});

it.each(['saved', 'main-roundtrip'] as const)('preserves %s metadata during an older full snapshot', async source => {
  const { result } = renderHook(() => useDetachedNote('own'));
  await flush();
  const oldFolders = deferred<FolderMeta[]>();
  native.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return session;
    if (command === 'list_notes') return [note('own', 'Before'), note('wiki', 'Wiki target')];
    if (command === 'list_folders') return oldFolders.promise;
    throw new Error(command);
  });
  await act(async () => native.focus?.({ payload: true }));
  await act(async () => {
    if (source === 'saved') result.current.applySavedNoteMeta(note('own', 'Fresh'));
    else native.listeners.get('note-meta-updated')!({ payload: { ...session, meta: note('own', 'Fresh') } });
  });
  await act(async () => oldFolders.resolve([folder('Before folder')]));
  await flush();
  expect(result.current.meta?.title).toBe('Fresh');
  expect(result.current.notes.find(n => n.id === 'own')?.title).toBe('Fresh');
  expect(result.current.notes.find(n => n.id === 'wiki')?.title).toBe('Wiki target');
});

it('discards a targeted result after the same profile returns with a newer generation', async () => {
  const { result } = renderHook(() => useDetachedNote('own'));
  await flush();
  const pending = deferred<NoteMeta[]>();
  let current = session;
  native.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return current;
    if (command === 'read_note_metadata_batch') return pending.promise;
    throw new Error(command);
  });
  await act(async () => {
    native.listeners.get('note-service-changed')!({ payload: { ...session, note_id: 'own' } });
    await vi.advanceTimersByTimeAsync(25);
  });
  current = { ...session, session_generation: 6 };
  await act(async () => pending.resolve([note('own', 'Stale session')]));
  await flush();
  expect(result.current.meta?.title).toBe('Before');
});
