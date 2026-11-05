// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listeners: new Map<string, Set<(event: { payload: unknown }) => void>>() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: ipc.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn(),
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    const listeners = ipc.listeners.get(name) ?? new Set();
    ipc.listeners.set(name, listeners); listeners.add(callback);
    return () => { listeners.delete(callback); };
  }),
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ setTitle: vi.fn(async () => {}), onFocusChanged: vi.fn(async () => () => {}) }) }));
vi.mock('@/lib/sync/queue', () => ({ getQueueSession: vi.fn() }));
vi.mock('@/i18n', () => ({ default: { t: (key: string) => key } }));
vi.mock('@/lib/sync/notes-store', () => ({
  createNoteSynced: vi.fn(), deleteNoteSynced: vi.fn(), toggleNoteFavoriteSynced: vi.fn(),
  moveNoteToFolderSynced: vi.fn(), pushNoteUpdate: vi.fn(), scheduleNoteUpdatePush: vi.fn(),
  cancelNoteUpdatePush: vi.fn(), setNoteLocalOnlySynced: vi.fn(), UPDATE_NOTE_PUSH_DEBOUNCE_MS: 2000,
}));
import { useNotes } from './useNotes';
import { useDetachedNote } from './useDetachedNote';

const session = { profile_id: 'work', session_generation: 5 };
const note = { id: 'own', title: 'Before', createdAt: 'now', updatedAt: 'now', favorite: false, order: 0 };
function emit(name: string, payload: unknown) { ipc.listeners.get(name)?.forEach(callback => callback({ payload })); }
beforeEach(() => {
  ipc.listeners.clear(); ipc.invoke.mockReset();
  ipc.invoke.mockImplementation(async (command: string, args?: { noteIds: string[] }) => {
    if (command === 'get_profile_session') return session;
    if (command === 'list_notes') return [note];
    if (command === 'list_folders') return [];
    if (command === 'read_note_metadata_batch') return (args?.noteIds ?? []).map(id => ({ ...note, id, title: 'Fresh' }));
    throw Error(`Unexpected command: ${command}`);
  });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

it('main note event bursts read targeted metadata without another full scan or any content read', async () => {
  const { result } = renderHook(useNotes);
  await waitFor(() => expect(ipc.listeners.get('note-service-changed')?.size).toBe(1));
  await waitFor(() => expect(result.current.notes).toHaveLength(1));
  ipc.invoke.mockClear(); vi.useFakeTimers();
  act(() => { for (let i = 0; i < 500; i++) emit('note-service-changed', { profile_id: 'work', note_id: `n${i % 225}` }); });
  await act(async () => { await vi.runAllTimersAsync(); });
  expect(ipc.invoke.mock.calls.filter(([command]) => command === 'read_note_metadata_batch')).toHaveLength(3);
  expect(ipc.invoke.mock.calls.some(([command]) => command === 'list_notes' || command === 'read_note' || command === 'list_folders')).toBe(false);
  expect(result.current.notes).toHaveLength(226);
});

it('detached autosaves update own metadata and wiki links; folder events refresh folders alone', async () => {
  const { result } = renderHook(() => useDetachedNote('own'));
  await waitFor(() => expect(result.current.meta?.title).toBe('Before'));
  await waitFor(() => expect(ipc.listeners.get('note-service-changed')?.size).toBe(1));
  ipc.invoke.mockClear(); vi.useFakeTimers();
  act(() => { for (let i = 0; i < 500; i++) emit('note-service-changed', { profile_id: 'work', note_id: 'own' }); });
  await act(async () => { await vi.runAllTimersAsync(); });
  expect(result.current.meta?.title).toBe('Fresh');
  expect(result.current.notes[0].title).toBe('Fresh');
  expect(result.current.loadFailed).toBe(false);
  expect(ipc.invoke.mock.calls.filter(([command]) => command === 'read_note_metadata_batch')).toHaveLength(1);
  expect(ipc.invoke.mock.calls.some(([command]) => command === 'list_notes' || command === 'read_note' || command === 'list_folders')).toBe(false);
  ipc.invoke.mockClear();
  act(() => { for (let i = 0; i < 500; i++) emit('folder-service-changed', { profile_id: 'work' }); });
  await act(async () => { await vi.runAllTimersAsync(); });
  expect(ipc.invoke.mock.calls.filter(([command]) => command === 'list_folders')).toHaveLength(1);
  expect(ipc.invoke.mock.calls.some(([command]) => command === 'list_notes' || command === 'read_note' || command === 'read_note_metadata_batch')).toBe(false);
});

it('detached deleted metadata marks the note unavailable without touching editor content', async () => {
  const { result } = renderHook(() => useDetachedNote('own'));
  await waitFor(() => expect(result.current.meta?.id).toBe('own'));
  ipc.invoke.mockImplementation(async (command: string) => {
    if (command === 'get_profile_session') return session;
    if (command === 'read_note_metadata_batch') return [];
    throw Error(`Unexpected command: ${command}`);
  });
  ipc.invoke.mockClear(); vi.useFakeTimers();
  act(() => emit('note-service-changed', { profile_id: 'work', note_id: 'own' }));
  await act(async () => { await vi.runAllTimersAsync(); });
  expect(result.current.loadFailed).toBe(true);
  expect(result.current.meta).toBeNull();
  expect(result.current.notes).toEqual([]);
  expect(ipc.invoke.mock.calls.some(([command]) => command === 'read_note')).toBe(false);
});
