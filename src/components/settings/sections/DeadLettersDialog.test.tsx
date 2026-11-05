// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import type { QueueSnapshot } from "@/lib/sync/queue";

const h = vi.hoisted(() => ({ snapshot: null as QueueSnapshot | null, revision: 0, refresh: vi.fn(), syncNow: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: async (command: string, args?: Record<string, unknown>) => {
  if (command === "get_profile_session") return { profile_id: "work", session_generation: 1 };
  const current = h.snapshot ?? { queue: [], deadLetters: [], importedOutboxSequence: 0, revision: "0" };
  if (command === "load_sync_queue") return structuredClone(current);
  if (command === "save_sync_queue") {
    if (args?.expectedRevision !== current.revision) throw Error("SYNC_QUEUE_REVISION_CONFLICT");
    h.snapshot = { ...structuredClone(args.snapshot as QueueSnapshot), revision: String(++h.revision) };
    return structuredClone(h.snapshot);
  }
  throw Error(command);
} }));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("@/hooks/useSync", () => ({ useSync: () => ({ refreshDeadLetters: h.refresh, syncNow: h.syncNow }) }));
vi.mock("@/components/ui/dialog", () => Object.fromEntries(
  ["Dialog", "DialogContent", "DialogDescription", "DialogHeader", "DialogTitle"].map(name => [name, ({ children }: { children: ReactNode }) => <div>{children}</div>]),
));

import { DeadLettersDialog } from "./DeadLettersDialog";
import { __resetForTests, dequeueById, enqueue, getDeadLetters, importOutboxEntries, moveToDeadLetter, peekAll } from "@/lib/sync/queue";
beforeEach(() => { h.snapshot = null; h.revision = 0; h.refresh.mockReset().mockResolvedValue(undefined); h.syncNow.mockReset().mockResolvedValue(undefined); __resetForTests(); });
afterEach(cleanup);

describe("stale DLQ dialog", () => {
  it.each(["deletion", "privacy transition"])("Retry cannot recreate captured content after a confirmed %s", async () => {
    const id = "11111111-1111-4111-8111-111111111111";
    const failed = await enqueue({ kind: "note-upsert", note: { id, title: "old", content_html: "captured old content", folder_id: null, favorite: false, order: 0, updated_at: "2026-11-05T00:00:00Z", deleted_at: null } });
    await moveToDeadLetter(failed.id, "offline");
    render(<DeadLettersDialog open onClose={() => {}} />);
    const retry = await screen.findByRole("button", { name: "sync.deadLetters.retry" });
    const deletion = { ...failed, id: "outbox:work:note:2", operation: { kind: "note-delete" as const, id } };
    await importOutboxEntries([deletion], 2, new Set([`note:${id}`]));
    await dequeueById(deletion.id);
    fireEvent.click(retry);
    await waitFor(() => expect(h.syncNow).toHaveBeenCalledOnce());
    expect(await peekAll()).toEqual([]); expect(await getDeadLetters()).toEqual([]);
    expect(h.snapshot!.importedOutboxSequence).toBe(2);
    expect(JSON.stringify(h.snapshot)).not.toContain("captured old content");
  });
});
