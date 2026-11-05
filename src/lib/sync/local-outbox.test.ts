import { beforeEach, describe, expect, it, vi } from "vitest";
import type { QueueSnapshot } from "./queue";
const state = vi.hoisted(() => ({ active: true, profile: "work", generation: 1, ackFails: false, saveFails: false, ack: 0, batches: [] as unknown[], snapshot: null as QueueSnapshot | null }));
vi.mock("./sync-gate", () => ({ isSyncActive: () => state.active }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async (cmd: string, args: Record<string, unknown> = {}) => {
  if (cmd === "get_profile_session") return { profile_id: state.profile, session_generation: state.generation };
  const session = args.session as { profile_id: string; session_generation: number };
  if (session.profile_id !== state.profile || session.session_generation !== state.generation) throw Error("EDITOR_SESSION_EXPIRED");
  if (cmd === "load_sync_queue") return structuredClone(state.snapshot ?? { queue: [], deadLetters: [], protectedEntities: [], importedOutboxSequence: 0, revision: "0" });
  if (cmd === "seed_local_sync_snapshot") return;
  if (cmd === "save_sync_queue") {
    if (state.saveFails) throw Error("SAVE_FAILED");
    state.snapshot = structuredClone(args.snapshot as QueueSnapshot); state.snapshot.revision += "1"; return state.snapshot;
  }
  if (cmd === "list_local_mutations") return state.batches.shift() ?? { profile_id: state.profile, through_sequence: args.afterSequence, entities: [] };
  if (cmd === "ack_local_mutations") { if (state.ackFails) throw Error("ACK_FAILED"); state.ack = args.throughSequence as number; return; }
  throw Error(cmd);
}) }));
import { drainLocalOutbox, protectedEntityKeys } from "./local-outbox";
import { retryDeadLetter } from "./queue";
import { __resetForTests, dequeueById, enqueue, getDeadLetters, moveToDeadLetter, peekAll, removeDeadLetter } from "./queue";
const id = "11111111-1111-4111-8111-111111111111";
function batch(sequence = 1, extra = {}) { return { profile_id: "work", through_sequence: sequence, entities: [{ kind: "note", id, sequence, deleted: false, cloud_delete: false, note: { meta: { id, title: "Title", createdAt: "2026-11-05T00:00:00Z", updatedAt: "2026-11-05T00:00:00Z", favorite: false, order: 0 }, content: "<p>latest</p>" }, ...extra }] }; }
beforeEach(() => { Object.assign(state, { active: true, profile: "work", generation: 1, ackFails: false, saveFails: false, ack: 0, batches: [], snapshot: null }); __resetForTests(); });
describe("local outbox", () => {
  it("inactive_profile_stays_pending", async () => { state.batches.push(batch()); expect((await drainLocalOutbox("personal")).state).toBe("deferred_profile_inactive"); expect(state.ack).toBe(0); expect(await peekAll()).toEqual([]); });
  it("disabled_sync_is_not_enabled_by_mcp", async () => { state.active = false; state.batches.push(batch()); expect((await drainLocalOutbox("work")).state).toBe("disabled"); expect(state.active).toBe(false); expect(state.batches).toHaveLength(1); });
  it("crash_after_enqueue_before_ack_is_deduplicated", async () => { state.batches.push(batch()); state.ackFails = true; await expect(drainLocalOutbox("work")).rejects.toThrow("ACK_FAILED"); state.ackFails = false; await drainLocalOutbox("work"); expect(await peekAll()).toHaveLength(1); expect(state.ack).toBe(1); });
  it("consumed_queue_entry_before_outbox_ack_is_not_reimported", async () => { state.batches.push(batch()); state.ackFails = true; await expect(drainLocalOutbox("work")).rejects.toThrow(); await dequeueById((await peekAll())[0].id); __resetForTests(); state.ackFails = false; await drainLocalOutbox("work"); expect(await peekAll()).toEqual([]); expect(state.ack).toBe(1); });
  it("failed_queue_save_never_acknowledges", async () => { state.saveFails = true; state.batches.push(batch()); await expect(drainLocalOutbox("work")).rejects.toThrow("SAVE_FAILED"); expect(state.ack).toBe(0); });
  it("delete_supersedes_pending_upsert_and_dlq", async () => { const old = await enqueue({ kind: "note-delete", id }); await moveToDeadLetter(old.id, "offline"); state.batches.push(batch(2, { deleted: true, note: null })); await drainLocalOutbox("work"); expect(await getDeadLetters()).toEqual([]); expect((await peekAll()).map(e => e.operation)).toEqual([{ kind: "note-delete", id }]); });
  it("local_only_never_enqueues_content", async () => { state.batches.push(batch(1, { note: null })); await drainLocalOutbox("work"); expect(await peekAll()).toEqual([]); expect(JSON.stringify(state.snapshot)).not.toContain("latest"); });
  it("private_cloud_delete_survives_local_only_gate", async () => { state.batches.push(batch(1, { note: null, cloud_delete: true })); await drainLocalOutbox("work"); expect((await peekAll())[0].operation).toEqual({ kind: "note-delete", id }); });
  it.each(["delete", "private"])("stale dialog retry cannot replay old content after consumed %s intent", async (intent) => {
    state.batches.push(batch()); await drainLocalOutbox("work");
    await moveToDeadLetter((await peekAll())[0].id, "offline");
    const dialogItem = (await getDeadLetters())[0];
    state.batches.push(batch(2, { deleted: intent === "delete", cloud_delete: intent === "private", note: null }));
    await drainLocalOutbox("work"); await dequeueById((await peekAll())[0].id);
    expect(await retryDeadLetter(dialogItem.id)).toBeNull();
    expect(await peekAll()).toEqual([]); expect(state.snapshot!.importedOutboxSequence).toBe(2);
    expect(JSON.stringify(state.snapshot)).not.toContain("latest");
  });
  it.each(["delete", "private"])("drain supersedes retried DLQ with a newly pending %s intent before dispatch", async (intent) => {
    state.batches.push(batch()); await drainLocalOutbox("work");
    await moveToDeadLetter((await peekAll())[0].id, "offline");
    const dialogItem = (await getDeadLetters())[0];
    state.batches.push(batch(2, { deleted: intent === "delete", cloud_delete: intent === "private", note: null }));
    await retryDeadLetter(dialogItem.id);
    // This is the production flush boundary: native intent is drained before
    // reading the operations to dispatch, even when Retry won the first CAS.
    await drainLocalOutbox("work");
    expect((await peekAll()).map(entry => entry.operation)).toEqual([{ kind: "note-delete", id }]);
    expect(JSON.stringify(await peekAll())).not.toContain("latest");
  });
  it("later_private_edit_retains_an_imported_cloud_delete", async () => { state.batches.push(batch(1, { note: null, cloud_delete: true })); await drainLocalOutbox("work"); state.batches.push(batch(2, { note: null })); await drainLocalOutbox("work"); expect((await peekAll())[0].operation).toEqual({ kind: "note-delete", id }); });
  it("legacy_seed_refreshes_latest_content_but_preserves_dlq_until_new_intent", async () => { const old = await enqueue({ kind: "note-delete", id }); await moveToDeadLetter(old.id, "offline"); state.batches.push(batch(1, { seed: true })); await drainLocalOutbox("work"); expect(await peekAll()).toEqual([]); expect((await getDeadLetters())[0].operation).toEqual(expect.objectContaining({ kind: "note-upsert", note: expect.objectContaining({ content_html: "<p>latest</p>" }) })); state.batches.push(batch(2)); await drainLocalOutbox("work"); expect(await getDeadLetters()).toEqual([]); expect(await peekAll()).toHaveLength(1); });
  it("invalid_legacy_folder_keeps_its_note_protected_without_poisoning_batch", async () => { const b = batch(); Object.assign(b.entities[0].note!.meta, { folderId: "old-folder" }); state.batches.push(b); await drainLocalOutbox("work"); expect(await peekAll()).toEqual([]); expect((await protectedEntityKeys()).has(`note:${id}`)).toBe(true); });
  it.each([{ name: "x".repeat(201), icon: null }, { name: "Folder", icon: "😀".repeat(17) }, { name: "", icon: null }])("invalid_folder_payload_stays_protected_without_poisoning_batch", async (folder) => {
    state.batches.push({ profile_id: "work", through_sequence: 1, entities: [{ kind: "folder", id, sequence: 1, deleted: false, cloud_delete: false, folder: { id, updatedAt: "2026-11-05", createdAt: "2026-11-05", order: 0, ...folder } }] });
    await drainLocalOutbox("work"); expect(await peekAll()).toEqual([]); expect((await protectedEntityKeys()).has(`folder:${id}`)).toBe(true); expect(state.snapshot!.protectedEntities![0].reason).toBe("invalid_folder");
  });
  it("disable_logout_and_restart_keep_pending_intent", async () => { state.batches.push(batch()); state.active = false; await drainLocalOutbox("work"); __resetForTests(); state.active = true; await drainLocalOutbox("work"); expect(await peekAll()).toHaveLength(1); expect(state.ack).toBe(1); });
  it.each(["", "é".repeat(1_572_865)])("unpushable_note_stays_protected_without_cloud_payload", async (content) => { const b = batch(); b.entities[0].note!.content = content; state.batches.push(b); await drainLocalOutbox("work"); expect(await peekAll()).toEqual([]); expect((await protectedEntityKeys()).has(`note:${id}`)).toBe(true); expect(JSON.stringify(state.snapshot).length).toBeLessThan(1000); });
  it("network_failure_or_dlq_keeps_pending_entity_protected_from_pull", async () => { state.batches.push(batch()); await drainLocalOutbox("work"); expect((await protectedEntityKeys()).has(`note:${id}`)).toBe(true); await moveToDeadLetter((await peekAll())[0].id, "offline"); expect((await protectedEntityKeys()).has(`note:${id}`)).toBe(true); });
  it("discarding_dlq_releases_protection_for_next_remote_reconciliation", async () => { state.batches.push(batch()); await drainLocalOutbox("work"); await moveToDeadLetter((await peekAll())[0].id, "offline"); await removeDeadLetter((await getDeadLetters())[0].id); expect((await protectedEntityKeys()).has(`note:${id}`)).toBe(false); expect(state.snapshot!.importedOutboxSequence).toBe(1); });
  it("loading_work_drains_before_its_pull_without_changing_personal_queue", async () => {
    state.profile = "personal"; await enqueue({ kind: "dictionary-upsert", word: "personal" }); const personal = structuredClone(state.snapshot);
    state.profile = "work"; state.snapshot = null; __resetForTests(); state.batches.push(batch()); await drainLocalOutbox("work");
    expect((await protectedEntityKeys()).has(`note:${id}`)).toBe(true); const work = structuredClone(state.snapshot as QueueSnapshot | null);
    state.profile = "personal"; state.snapshot = personal; __resetForTests(); expect((await peekAll()).map(e => e.operation)).toEqual([{ kind: "dictionary-upsert", word: "personal" }]); expect(work!.queue).toHaveLength(1);
  });
});
