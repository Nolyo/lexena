import { describe, it, expect, vi } from "vitest";

const invokeMock = vi.fn(async (cmd: string): Promise<unknown> => {
  if (cmd === "get_active_profile") return "default";
  if (cmd === "list_profiles")
    return [{ id: "default", name: "Perso", createdAt: "" }];
  throw new Error(`unexpected ${cmd}`);
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: (c: string) => invokeMock(c) }));

import { ensureCloudProfileId } from "./cloud-profile";

describe("ensureCloudProfileId", () => {
  it("generates and persists an id when absent, returns the active name", async () => {
    const store: Record<string, unknown> = {};
    const getMeta = async <T,>(k: string, d: T) => (store[k] as T) ?? d;
    const setMeta = async (k: string, v: unknown) => {
      store[k] = v;
    };

    const first = await ensureCloudProfileId(getMeta, setMeta);
    // UUID v4 pattern: 8-4-4-4-12 hex chars with version=4 and variant bits
    expect(first.id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
    );
    expect(first.name).toBe("Perso");

    // Id must be persisted so subsequent calls return the same value
    const second = await ensureCloudProfileId(getMeta, setMeta);
    expect(second.id).toBe(first.id); // stable, reused
  });

  it("falls back to 'Profil' when active profile is not found in list", async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    invokeMock.mockImplementation(async (cmd: string): Promise<any> => {
      if (cmd === "get_active_profile") return "unknown-id";
      if (cmd === "list_profiles") return [{ id: "default", name: "Main", createdAt: "" }];
      throw new Error(`unexpected ${cmd}`);
    });

    const store: Record<string, unknown> = {};
    const getMeta = async <T,>(k: string, d: T) => (store[k] as T) ?? d;
    const setMeta = async (k: string, v: unknown) => {
      store[k] = v;
    };

    const result = await ensureCloudProfileId(getMeta, setMeta);
    expect(result.name).toBe("Profil");
  });
  it("uses the captured profile rather than a later active profile", async () => {
    invokeMock.mockImplementation(async (cmd: string): Promise<unknown> => {
      if (cmd === "get_active_profile") throw Error("active lookup must not happen");
      if (cmd === "list_profiles") return [{ id: "work", name: "Work", createdAt: "" }, { id: "personal", name: "Personal", createdAt: "" }];
      throw Error(cmd);
    });
    expect(await ensureCloudProfileId(async <T,>(_key: string, fallback: T) => fallback, async () => {}, "work")).toEqual({ id: expect.any(String), name: "Work" });
    await expect(ensureCloudProfileId(async <T,>(_key: string, fallback: T) => fallback, async () => {}, "deleted")).rejects.toThrow("EDITOR_SESSION_EXPIRED");
  });
});
