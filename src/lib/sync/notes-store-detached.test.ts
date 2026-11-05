import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());
const wake = vi.hoisted(() => vi.fn());
vi.mock("./local-outbox", () => ({ requestOutboxDrain: wake }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn() }));

import {
  cancelNoteUpdatePush,
  scheduleNoteUpdatePushFromDisk,
} from "./notes-store";

describe("scheduleNoteUpdatePushFromDisk", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invokeMock.mockReset();
    wake.mockReset();
    invokeMock.mockResolvedValue({
      meta: { id: "n1", title: "T", updatedAt: "2026-07-24T00:00:00Z" },
      content: "<p>x</p>",
    });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("wakes the native current-state drain only when the debounce fires", async () => {
    scheduleNoteUpdatePushFromDisk("n1", 2000);
    expect(invokeMock).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(2000);
    expect(invokeMock).not.toHaveBeenCalled();
    expect(wake).toHaveBeenCalledOnce();
  });

  it("coalesces rapid re-schedules into a single read", async () => {
    scheduleNoteUpdatePushFromDisk("n1", 2000);
    await vi.advanceTimersByTimeAsync(1000);
    scheduleNoteUpdatePushFromDisk("n1", 2000);
    await vi.advanceTimersByTimeAsync(2000);
    expect(wake).toHaveBeenCalledOnce();
  });

  it("cancelNoteUpdatePush drops the pending push", async () => {
    scheduleNoteUpdatePushFromDisk("n1", 2000);
    cancelNoteUpdatePush("n1");
    await vi.advanceTimersByTimeAsync(3000);
    expect(invokeMock).not.toHaveBeenCalled();
    expect(wake).not.toHaveBeenCalled();
  });
});
