/** Apply profile content while isolating failures of individual rows. */
export function normalizePulledNoteFolders<Folder extends { id: string; deleted_at: string | null }, Note extends { folder_id: string | null; deleted_at: string | null }>(folders: readonly Folder[], notes: readonly Note[], survivingActiveFolderIds: ReadonlySet<string>): Note[] {
  const deleted = new Set(folders.filter((folder) => folder.deleted_at !== null).map((folder) => folder.id));
  return notes.map((note) => note.deleted_at === null && note.folder_id !== null && deleted.has(note.folder_id) && !survivingActiveFolderIds.has(note.folder_id)
    ? { ...note, folder_id: null }
    : note);
}

export async function applyContentInDependencyOrder<Folder extends { id: string }, Note extends { id: string }>(
  folders: readonly Folder[], notes: readonly Note[],
  applyFolder: (row: Folder) => Promise<void>, applyNote: (row: Note) => Promise<void>,
  onError: (kind: "folder" | "note", id: string, error: unknown) => void,
  prepareNotes: (rows: readonly Note[]) => Promise<readonly Note[]> = async (rows) => rows,
): Promise<void> {
  // Notes can reference these folders even when this device has no local data.
  for (const row of folders) {
    try { await applyFolder(row); } catch (error) { onError("folder", row.id, error); }
  }
  for (const row of await prepareNotes(notes)) {
    try { await applyNote(row); } catch (error) { onError("note", row.id, error); }
  }
}
