import { describe, it, expect, beforeEach, vi } from "vitest";

// ── Mocks ────────────────────────────────────────────────────────────────────
//
// We mock @tauri-apps/api/core to intercept invoke() and @tauri-apps/plugin-store
// to provide an in-memory Tauri Store. The store mock returns a fresh object per
// `load()` call, which is enough for these tests because we never reload mid-run.

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const storeData: Record<string, Record<string, unknown>> = {};
vi.mock("@tauri-apps/plugin-store", () => ({
  Store: {
    load: async (file: string) => {
      if (!storeData[file]) storeData[file] = {};
      const d = storeData[file];
      return {
        entries: async () =>
          Object.entries(d) as Array<[string, unknown]>,
        keys: async () => Object.keys(d),
        set: async (k: string, v: unknown) => {
          d[k] = v;
        },
        delete: async (k: string) => {
          delete d[k];
        },
        save: async () => {},
      };
    },
  },
}));

import { invoke } from "@tauri-apps/api/core";
import { __resetForTests as resetQueueSession } from "./queue";
import {
  createLocalBackup,
  restoreLocalBackup,
  type BackupPayloadV2,
  type BackupPayloadV1,
} from "./backups";

const invokeMock = vi.mocked(invoke);

beforeEach(() => {
  invokeMock.mockReset();
  resetQueueSession();
  for (const k of Object.keys(storeData)) delete storeData[k];
});

// ── createLocalBackup ────────────────────────────────────────────────────────

describe("createLocalBackup", () => {
  it("snapshots stores + notes (via list_notes + read_note per note) + folders, writes v2 payload", async () => {
    // Seed a fake store so snapshotStores returns something non-trivial.
    storeData["settings.json"] = { theme: "dark" };

    const fakeNotes = [
      { id: "n1", title: "Note 1", createdAt: "c1", updatedAt: "u1", favorite: false, order: 0 },
      { id: "n2", title: "Note 2", createdAt: "c2", updatedAt: "u2", favorite: true, order: 1 },
    ];
    const fakeFolders = [
      { id: "f1", name: "Work", createdAt: "c", updatedAt: "u", order: 0 },
    ];

    let writtenJson = "";

    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const a = args as Record<string, unknown> | undefined;
      switch (cmd) {
        case "list_notes":
          return fakeNotes;
        case "read_note": {
          const id = a?.id as string;
          const meta = fakeNotes.find((n) => n.id === id);
          return { meta, content: `<p>${id}</p>` };
        }
        case "list_folders":
          return fakeFolders;
        case "write_local_backup":
          writtenJson = a?.payloadJson as string;
          return "pre-sync_2026-05-19_120000.json";
        default:
          throw new Error(`unexpected invoke: ${cmd}`);
      }
    });

    const filename = await createLocalBackup();
    expect(filename).toBe("pre-sync_2026-05-19_120000.json");

    // list_notes + 2× read_note + list_folders + write_local_backup
    const calls = invokeMock.mock.calls.map(([c]) => c);
    expect(calls).toContain("list_notes");
    expect(calls.filter((c) => c === "read_note")).toHaveLength(2);
    expect(calls).toContain("list_folders");
    expect(calls).toContain("write_local_backup");

    const payload = JSON.parse(writtenJson) as BackupPayloadV2;
    expect(payload.version).toBe(2);
    expect(payload.notes).toHaveLength(2);
    expect(payload.notes[0].meta.id).toBe("n1");
    expect(payload.notes[0].content).toBe("<p>n1</p>");
    expect(payload.folders).toEqual(fakeFolders);
    expect(payload.tauri_stores["settings.json"]).toEqual({ theme: "dark" });
  });

  it("skips notes that fail to read but still includes the others", async () => {
    const fakeNotes = [
      { id: "ok", title: "OK", createdAt: "c", updatedAt: "u", favorite: false, order: 0 },
      { id: "broken", title: "X", createdAt: "c", updatedAt: "u", favorite: false, order: 1 },
    ];
    let writtenJson = "";

    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const a = args as Record<string, unknown> | undefined;
      switch (cmd) {
        case "list_notes":
          return fakeNotes;
        case "read_note": {
          const id = a?.id as string;
          if (id === "broken") throw new Error("disk error");
          return { meta: fakeNotes[0], content: "<p>ok</p>" };
        }
        case "list_folders":
          return [];
        case "write_local_backup":
          writtenJson = a?.payloadJson as string;
          return "pre-sync_x.json";
        default:
          throw new Error(`unexpected invoke: ${cmd}`);
      }
    });

    await createLocalBackup();
    const payload = JSON.parse(writtenJson) as BackupPayloadV2;
    expect(payload.notes).toHaveLength(1);
    expect(payload.notes[0].meta.id).toBe("ok");
  });
});

// ── restoreLocalBackup ───────────────────────────────────────────────────────

describe("restoreLocalBackup", () => {
  it("pins the original profile session before reading a delayed backup and forwards it to every import", async () => {
    const session = { profile_id: "work", session_generation: 7 };
    let current = session;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_profile_session") return current;
      if (command === "read_local_backup") {
        current = { profile_id: "personal", session_generation: 8 };
        return JSON.stringify({version:2,created_at:"now",tauri_stores:{},folders:[],notes:[{meta:{id:"a"},content:"backup"}]});
      }
      if (command.startsWith("import_")) return null;
      throw Error(`unexpected ${command}`);
    });
    await restoreLocalBackup("backup.json");
    expect(invokeMock.mock.calls[0][0]).toBe("get_profile_session");
    expect(invokeMock).toHaveBeenCalledWith("import_note_for_backup", expect.objectContaining({session,source:"backup"}));
    expect(invokeMock).toHaveBeenCalledWith("import_folders_for_backup", expect.objectContaining({session,source:"backup"}));
  });
  it("restores referenced folders before their notes on a fresh profile", async () => {
    const payload: BackupPayloadV2 = {
      version: 2,
      created_at: "2026-11-05T10:00:00Z",
      tauri_stores: {},
      folders: [{ id: "folder", name: "Projects", createdAt: "c", updatedAt: "u", order: 0 }],
      notes: [{ meta: { id: "note", title: "Project", folderId: "folder", createdAt: "c", updatedAt: "u", favorite: false, order: 0 }, content: "<p>Keep this</p>" }],
    };
    const folders = new Set<string>();
    const persisted: string[] = [];
    invokeMock.mockImplementation(async (command, args) => {
      if (command === "get_profile_session") return {profile_id:"work",session_generation:7};
      if (command === "read_local_backup") return JSON.stringify(payload);
      if (command === "import_folders_for_backup") {
        for (const folder of (args as { folders: BackupPayloadV2["folders"] }).folders) folders.add(folder.id);
        return;
      }
      if (command === "import_note_for_backup") {
        const note = args as unknown as BackupPayloadV2["notes"][number];
        if (note.meta.folderId && !folders.has(note.meta.folderId)) throw new Error("Folder not found");
        persisted.push(note.content);
        return;
      }
      throw new Error(`unexpected invoke: ${command}`);
    });

    const result = await restoreLocalBackup("profile.json");
    expect(result.failed).toEqual([]);
    expect(result.notes_restored).toBe(1);
    expect(persisted).toEqual(["<p>Keep this</p>"]);
  });

  it("v1 backup: restores stores only — no import_note / import_folders calls", async () => {
    const v1: BackupPayloadV1 = {
      version: 1,
      created_at: "2026-05-19T10:00:00Z",
      tauri_stores: { "settings.json": { theme: "light" } },
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_profile_session") return {profile_id:"work",session_generation:7};
      if (cmd === "read_local_backup") return JSON.stringify(v1);
      throw new Error(`unexpected invoke: ${cmd}`);
    });

    const result = await restoreLocalBackup("pre-sync_v1.json");
    expect(result.notes_restored).toBe(0);
    expect(result.folders_restored).toBe(0);

    const calls = invokeMock.mock.calls.map(([c]) => c);
    expect(calls).not.toContain("import_note_for_backup");
    expect(calls).not.toContain("import_folders_for_backup");

    // The store should now hold the restored key.
    expect(storeData["settings.json"]).toEqual({ theme: "light" });
  });

  it("v2 backup: calls import_note_for_backup per note + import_folders_for_backup once", async () => {
    const v2: BackupPayloadV2 = {
      version: 2,
      created_at: "2026-05-19T11:00:00Z",
      tauri_stores: {},
      notes: [
        { meta: { id: "n1", title: "A", createdAt: "c", updatedAt: "u", favorite: false, order: 0 }, content: "<p>a</p>" },
        { meta: { id: "n2", title: "B", createdAt: "c", updatedAt: "u", favorite: false, order: 1 }, content: "<p>b</p>" },
        { meta: { id: "n3", title: "C", createdAt: "c", updatedAt: "u", favorite: false, order: 2, deletedAt: "2026-05-19T09:00:00Z" }, content: "<p>c</p>" },
      ],
      folders: [
        { id: "f1", name: "Work", createdAt: "c", updatedAt: "u", order: 0 },
        { id: "f2", name: "Personal", createdAt: "c", updatedAt: "u", order: 1 },
      ],
    };

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_profile_session") return {profile_id:"work",session_generation:7};
      if (cmd === "read_local_backup") return JSON.stringify(v2);
      if (cmd === "import_note_for_backup") return null;
      if (cmd === "import_folders_for_backup") return null;
      throw new Error(`unexpected invoke: ${cmd}`);
    });

    const result = await restoreLocalBackup("pre-sync_v2.json");
    expect(result.notes_restored).toBe(3);
    expect(result.folders_restored).toBe(2);
    expect(result.failed).toEqual([]);

    const calls = invokeMock.mock.calls.map(([c]) => c);
    expect(calls.filter((c) => c === "import_note_for_backup")).toHaveLength(3);
    expect(calls.filter((c) => c === "import_folders_for_backup")).toHaveLength(1);

    // Spot-check: the tombstone on n3 was forwarded to import_note_for_backup intact.
    const importNoteCalls = invokeMock.mock.calls.filter(([c]) => c === "import_note_for_backup");
    const n3Call = importNoteCalls.find(([, args]) =>
      (args as { meta: { id: string } }).meta.id === "n3",
    );
    expect(n3Call).toBeDefined();
    const n3Args = n3Call![1] as { meta: { deletedAt?: string }; content: string };
    expect(n3Args.meta.deletedAt).toBe("2026-05-19T09:00:00Z");
    expect(n3Args.content).toBe("<p>c</p>");
  });

  it("per-note import error is reported in `failed` but doesn't block the rest", async () => {
    const v2: BackupPayloadV2 = {
      version: 2,
      created_at: "2026-05-19T11:00:00Z",
      tauri_stores: {},
      notes: [
        { meta: { id: "ok1", title: "A", createdAt: "c", updatedAt: "u", favorite: false, order: 0 }, content: "" },
        { meta: { id: "boom", title: "B", createdAt: "c", updatedAt: "u", favorite: false, order: 1 }, content: "" },
        { meta: { id: "ok2", title: "C", createdAt: "c", updatedAt: "u", favorite: false, order: 2 }, content: "" },
      ],
      folders: [],
    };

    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const a = args as Record<string, unknown> | undefined;
      if (cmd === "get_profile_session") return {profile_id:"work",session_generation:7};
      if (cmd === "read_local_backup") return JSON.stringify(v2);
      if (cmd === "import_note_for_backup") {
        const meta = a?.meta as { id: string };
        if (meta.id === "boom") throw new Error("write failed");
        return null;
      }
      if (cmd === "import_folders_for_backup") return null;
      throw new Error(`unexpected invoke: ${cmd}`);
    });

    const result = await restoreLocalBackup("pre-sync_v2.json");
    expect(result.notes_restored).toBe(2);
    expect(result.failed).toHaveLength(1);
    expect(result.failed[0].file).toBe("note:boom");
    expect(result.failed[0].error).toContain("write failed");

    // All three notes were attempted (loop didn't break on the failure).
    const importCalls = invokeMock.mock.calls.filter(([c]) => c === "import_note_for_backup");
    expect(importCalls).toHaveLength(3);
  });
});
