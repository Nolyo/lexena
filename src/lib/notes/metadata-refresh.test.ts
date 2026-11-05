import { afterEach, expect, it, vi } from 'vitest';
import { createScopedMetadataRefresh, mergeNoteMetadata } from './metadata-refresh';
import type { NoteMeta } from '@/hooks/useNotes';

const session = { profile_id: 'work', session_generation: 4 };
const meta = (id: string, title = id): NoteMeta => ({ id, title, createdAt: 'now', updatedAt: 'now', order: 0, favorite: false });
afterEach(() => vi.useRealTimers());

it('coalesces 500 events into deduplicated batches of at most 100 with one request in flight', async () => {
  vi.useFakeTimers();
  const requests: string[][] = [];
  let active = 0;
  let peak = 0;
  const apply = vi.fn();
  const refresh = createScopedMetadataRefresh({ session, currentSession: async () => session,
    fetch: async (ids) => { peak = Math.max(peak, ++active); requests.push(ids); await Promise.resolve(); active--; return ids.map(id => meta(id)); }, apply, onError: vi.fn() });
  for (let i = 0; i < 500; i++) refresh.enqueue({ profile_id: 'work', note_id: String(i % 225) });
  await vi.runAllTimersAsync();
  expect(requests.map(ids => ids.length)).toEqual([100, 100, 25]);
  expect(new Set(requests.flat()).size).toBe(225);
  expect(peak).toBe(1);
  expect(apply).toHaveBeenCalledTimes(3);
});

it('rereads events arriving during a pending request and never applies its stale snapshot', async () => {
  vi.useFakeTimers();
  let release!: (notes: NoteMeta[]) => void;
  const fetch = vi.fn().mockImplementationOnce(() => new Promise<NoteMeta[]>(done => { release = done; }))
    .mockResolvedValueOnce([meta('a', 'latest')]);
  let notes = [meta('a', 'before')];
  const appliedTitles: string[] = [];
  const refresh = createScopedMetadataRefresh<NoteMeta>({ session, currentSession: async () => session, fetch,
    apply: (ids, result) => { notes = mergeNoteMetadata(notes, ids, result); appliedTitles.push(notes[0].title); }, onError: vi.fn() });
  refresh.enqueue({ profile_id: 'work', note_id: 'a' });
  await vi.advanceTimersByTimeAsync(25);
  refresh.enqueue({ profile_id: 'work', note_id: 'a' });
  release([meta('a', 'stale')]);
  await vi.runAllTimersAsync();
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(notes[0].title).toBe('latest');
  expect(appliedTitles).toEqual(['latest']);
});

it.each(['different-profile', 'aba', 'disposed'] as const)('discards late results after %s', async kind => {
  vi.useFakeTimers();
  let release!: (notes: NoteMeta[]) => void;
  let current = session;
  const apply = vi.fn();
  const refresh = createScopedMetadataRefresh({ session, currentSession: async () => current,
    fetch: () => new Promise<NoteMeta[]>(done => { release = done; }), apply, onError: vi.fn() });
  refresh.enqueue({ profile_id: 'foreign', note_id: 'ignored' });
  refresh.enqueue({ profile_id: 'work', session_generation: 3, note_id: 'old-generation' });
  await vi.runAllTimersAsync();
  expect(release).toBeUndefined();
  refresh.enqueue({ profile_id: 'work', note_id: 'a' });
  await vi.advanceTimersByTimeAsync(25);
  if (kind === 'disposed') refresh.dispose();
  else current = { profile_id: kind === 'aba' ? 'work' : 'personal', session_generation: 6 };
  release([meta('a')]);
  await vi.runAllTimersAsync();
  expect(apply).not.toHaveBeenCalled();
  refresh.enqueue({ profile_id: 'work', note_id: 'b' });
  await vi.runAllTimersAsync();
  expect(apply).not.toHaveBeenCalled();
});

it('merges only requested metadata, adds creates, removes missing/deleted IDs, and preserves other entries', () => {
  const untouched = meta('untouched');
  const merged = mergeNoteMetadata([untouched, meta('deleted'), meta('missing'), meta('updated')],
    ['deleted', 'missing', 'updated', 'created'],
    [{ ...meta('deleted'), deletedAt: 'now' }, meta('updated', 'new title'), meta('created'), meta('unrequested')]);
  expect(merged.map(note => note.id).sort()).toEqual(['created', 'untouched', 'updated']);
  expect(merged.find(note => note.id === 'untouched')).toBe(untouched);
  expect(merged.find(note => note.id === 'updated')?.title).toBe('new title');
});
