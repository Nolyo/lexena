// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";

type Handler = (event: { payload: unknown }) => void;
const listeners = vi.hoisted(() => new Map<string, Handler>());
const session = {profile_id:"work",session_generation:3};
vi.mock("@tauri-apps/api/core", () => ({invoke:vi.fn(async () => ({profile_id:"work",session_generation:3}))}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: Handler) => {
    listeners.set(name, handler);
    return () => listeners.delete(name);
  }),
}));

import { useDetachedNotesBridge } from "./useDetachedNotesBridge";

function makeHandlers() {
  return {
    onWindowClosed: vi.fn(),
    onReattachRequest: vi.fn(),
    onOpenRequest: vi.fn(),
    onDeleteRequest: vi.fn(),
    onDetachedUpdated: vi.fn(),
    onToggleLocalOnlyRequest: vi.fn(),
  };
}

describe("useDetachedNotesBridge", () => {
  it("routes each event to its handler with the note id", async () => {
    const handlers = makeHandlers();
    renderHook(() => useDetachedNotesBridge(handlers));
    // listen() registrations are async — flush microtasks.
    await waitFor(() => expect(listeners.has("note-window-closed")).toBe(true));

    listeners.get("note-window-closed")!({ payload: { ...session,noteId: "n1" } });
    expect(handlers.onWindowClosed).toHaveBeenCalledWith("n1");

    listeners.get("note-reattach-request")!({ payload: { ...session,id: "n2" } });
    expect(handlers.onReattachRequest).toHaveBeenCalledWith("n2");

    listeners.get("note-open-request")!({ payload: { ...session,id: "n3" } });
    expect(handlers.onOpenRequest).toHaveBeenCalledWith("n3");

    listeners.get("note-detached-delete-request")!({ payload: { ...session,id: "n4" } });
    expect(handlers.onDeleteRequest).toHaveBeenCalledWith("n4");

    const updatePayload = { ...session,id: "n5", title: "T", updatedAt: "2026-07-24" };
    listeners.get("note-detached-updated")!({ payload: updatePayload });
    expect(handlers.onDetachedUpdated).toHaveBeenCalledWith(updatePayload);

    listeners.get("note-toggle-local-only-request")!({ payload: { ...session,id: "n6" } });
    expect(handlers.onToggleLocalOnlyRequest).toHaveBeenCalledWith("n6");
  });

  it("uses the LATEST handlers (ref pattern), not the mount-time ones", async () => {
    const first = makeHandlers();
    const { rerender } = renderHook(
      ({ h }) => useDetachedNotesBridge(h),
      { initialProps: { h: first } },
    );
    await waitFor(() => expect(listeners.has("note-window-closed")).toBe(true));
    const second = makeHandlers();
    rerender({ h: second });
    listeners.get("note-window-closed")!({ payload: { ...session,noteId: "n1" } });
    expect(first.onWindowClosed).not.toHaveBeenCalled();
    expect(second.onWindowClosed).toHaveBeenCalledWith("n1");
  });
  it("ignores detached actions from another profile or a previous A-B-A session",async()=>{
    const handlers=makeHandlers();renderHook(()=>useDetachedNotesBridge(handlers));
    await waitFor(()=>expect(listeners.has("note-detached-delete-request")).toBe(true));
    listeners.get("note-detached-delete-request")!({payload:{profile_id:"personal",session_generation:3,id:"same-id"}});
    listeners.get("note-detached-delete-request")!({payload:{profile_id:"work",session_generation:1,id:"same-id"}});
    expect(handlers.onDeleteRequest).not.toHaveBeenCalled();
  });
});
