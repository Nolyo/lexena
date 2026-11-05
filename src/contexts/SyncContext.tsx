import {
  createContext,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Store } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useAuth } from "@/hooks/useAuth";
import { useSettings } from "@/hooks/useSettings";
import {
  extractCloudSettings,
  syncableSettingsChanged,
} from "@/lib/sync/mapping";
import { flog } from "@/lib/flog";
import { pullAll, pushOperations } from "@/lib/sync/client";
import {
  enqueue,
  enqueueWithId,
  peekAll,
  peekReadyHead,
  markRetry,
  moveToDeadLetter,
  getDeadLetters,
  MAX_RETRIES_BEFORE_DLQ,
  size as queueSize,
  getQueueSession,
  loadSnapshot,
} from "@/lib/sync/queue";
import { drainLocalOutbox, protectedEntityKeys } from "@/lib/sync/local-outbox";
import { nextDependencyBatch } from "@/lib/sync/push-dependencies";
import { applyBatchResults } from "@/lib/sync/apply-batch-results";
import {
  loadSnippets,
  applyRemoteSnippet,
  migrateLegacySnippetsOnce,
} from "@/lib/sync/snippets-store";
import {
  applyRemoteNote,
  flushPendingNoteUpdates,
  scanOversizedNoteCount,
} from "@/lib/sync/notes-store";
import { applyRemoteFolder, listFolders } from "@/lib/sync/folders-store";
import { applyContentInDependencyOrder, normalizePulledNoteFolders } from "@/lib/sync/apply-content";
import {
  loadDictionary,
  applyRemoteWord,
  migrateLegacyDictionaryOnce,
} from "@/lib/sync/dictionary-store";
import { mergeSettingsLWW, shouldApplyCloudSettings } from "@/lib/sync/merge";
import { computeNextPullCursor } from "@/lib/sync/pull-cursor";
import { isSyncActive, setSyncActive } from "@/lib/sync/sync-gate";
import { ensureCloudProfileId } from "@/lib/sync/cloud-profile";
import type { AppSettings } from "@/lib/settings";
import type { SyncOperation, SyncState, SyncStatus } from "@/lib/sync/types";

const KEY_ENABLED = "enabled";
const KEY_LAST_PULL_AT = "last_pull_at";
const KEY_LAST_PUSHED_SETTINGS_AT = "last_pushed_settings_at";
const KEY_CLOUD_PROFILE_ID = "cloud_profile_id";
// True once the initial full push of local state has completed successfully for
// this profile. Until then, mount re-tries it — so a push that failed (offline,
// an oversized note that has since been trimmed, a transient 5xx) eventually
// completes on a later launch without the user having to toggle sync off/on.
const KEY_INITIAL_PUSH_DONE = "initial_push_done";
const DEBOUNCE_PUSH_MS = 500;
const FOCUS_PULL_IDLE_MS = 5 * 60 * 1000;

export interface SyncContextValue extends SyncState {
  dead_letter_count: number;
  enableSync(): Promise<void>;
  disableSync(): Promise<void>;
  syncNow(): Promise<void>;
  refreshDeadLetters(): Promise<void>;
  notifySettingsChanged(
    previous: AppSettings["settings"],
    current: AppSettings["settings"]
  ): void;
  notifyDictionaryUpserted(word: string): void;
  notifyDictionaryDeleted(word: string): void;
  notifySnippetUpserted(
    snippetId: string,
    label: string,
    content: string,
    shortcut: string | null
  ): void;
  notifySnippetDeleted(snippetId: string): void;
}

export const SyncContext = createContext<SyncContextValue | null>(null);

let metaStorePromise: Promise<Awaited<ReturnType<typeof Store.load>>> | null = null;
async function getMetaStore() {
  if (!metaStorePromise) {
    metaStorePromise = (async () => {
      const session = await getQueueSession();
      const path = await invoke<string>("get_profile_sync_meta_path", { id: session.profile_id });
      return Store.load(path);
    })();
  }
  return metaStorePromise;
}
async function getMeta<T>(key: string, def: T): Promise<T> {
  const store = await getMetaStore();
  const v = await store.get<T>(key);
  return v ?? def;
}
async function setMeta(key: string, value: unknown): Promise<void> {
  const store = await getMetaStore();
  await store.set(key, value);
  await store.save();
}

export function SyncProvider({ children }: { children: ReactNode }) {
  const auth = useAuth();
  const { settings, updateSettings, subscribeToChanges } = useSettings();
  const [enabled, setEnabled] = useState(false);
  const [status, setStatus] = useState<SyncStatus>("disabled");
  const [lastSyncAt, setLastSyncAt] = useState<string | null>(null);
  const [lastPullAt, setLastPullAt] = useState<string | null>(null);
  const [pendingCount, setPendingCount] = useState(0);
  const [deadLetterCount, setDeadLetterCount] = useState(0);
  const [oversizedNoteCount, setOversizedNoteCount] = useState(0);
  const [lastError, setLastError] = useState<string | null>(null);
  const debounceRef = useRef<number | null>(null);
  const flushingRef = useRef(false);
  const pushingRef = useRef(false);
  const pullingRef = useRef(false);
  const pullAgainRef = useRef(false);
  // Always-fresh mirror so stable callbacks (flush, pullAndApply, notify*) can
  // read the latest settings without re-subscribing on every settings change.
  const settingsRef = useRef<AppSettings["settings"]>(settings);
  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  // Charger enabled + last_pull_at au mount
  useEffect(() => {
    (async () => {
      const en = await getMeta<boolean>(KEY_ENABLED, false);
      const lp = await getMeta<string | null>(KEY_LAST_PULL_AT, null);
      setEnabled(en);
      setSyncActive(en);
      setLastPullAt(lp);
      setPendingCount(await queueSize());
      setDeadLetterCount((await getDeadLetters()).length);
      setStatus(en ? "idle" : "disabled");
    })();
  }, []);

  const getDeviceId = useCallback(async (): Promise<string> => {
    return invoke<string>("get_or_create_device_id");
  }, []);

  const refreshDeadLetterCount = useCallback(async () => {
    const dlq = await getDeadLetters();
    setDeadLetterCount(dlq.length);
  }, []);

  const flushQueue = useCallback(async () => {
    if (flushingRef.current || !isSyncActive() || auth.status !== "signed-in") return false;
    flushingRef.current = true;
    setStatus("syncing");
    try {
      const deviceId = await getDeviceId();
      const session = await getQueueSession();
      await drainLocalOutbox(session.profile_id);
      const { id: cloudProfileId, name } = await ensureCloudProfileId(getMeta, setMeta, session.profile_id);
      if ([...await peekAll(), ...await getDeadLetters()].some(e => e.operation.kind === "profile-delete" && e.operation.id === cloudProfileId)) return false;
      // Put parent confirmation through the same durable retry/DLQ flow. Its
      // dependency phase is sent alone, and failure blocks folders and notes.
      if ((await peekAll()).length > 0) await enqueueWithId(`profile-parent:${cloudProfileId}`, { kind: "profile-upsert", profile: { id: cloudProfileId, name, updated_at: new Date().toISOString(), deleted_at: null } });
      let sawError = false;
      // eslint-disable-next-line no-constant-condition
      while (true) {
        if (!isSyncActive()) break;
        await drainLocalOutbox(session.profile_id);
        const pending = await peekAll();
        const blockedFolders = new Set((await loadSnapshot()).protectedEntities?.filter(e => e.kind === "folder").map(e => e.id) ?? []);
        const batch = nextDependencyBatch(pending, await getDeadLetters(), Date.now(), blockedFolders);
        if (!batch.length) break;
        const ready = batch[0];
        const ops = batch.map((e) => e.operation);
        const resp = await pushOperations(ops, deviceId, cloudProfileId);
        if (!resp.ok) {
          // Whole-batch failure (network, 5xx, 413 quota)
          await markRetry(ready.id, resp.error ?? "push failed");
          const updated = (await peekAll()).find(e => e.id === ready.id);
          if (
            updated &&
            updated.id === ready.id &&
            updated.retry_count >= MAX_RETRIES_BEFORE_DLQ
          ) {
            await moveToDeadLetter(updated.id, resp.error ?? "max retries");
          }
          if (resp.status === 413) {
            setStatus("quota-exceeded");
          } else {
            setStatus("offline");
          }
          setLastError(resp.error ?? "push failed");
          sawError = true;
          break;
        }
        const { failedCount } = await applyBatchResults(batch, resp.results);

        // Move to DLQ if any entry crossed the retry threshold
        const after = await peekAll();
        for (const e of after) {
          if (e.retry_count >= MAX_RETRIES_BEFORE_DLQ) {
            await moveToDeadLetter(e.id, e.last_error ?? "max retries");
          }
        }

        if (failedCount > 0) {
          setStatus("error");
          setLastError("Some operations failed");
          sawError = true;
          break;
        }
        if (resp.server_time) {
          // Si le batch a poussé un settings-upsert, on note le server_time
          if (batch.some((e) => e.operation.kind === "settings-upsert")) {
            await setMeta(KEY_LAST_PUSHED_SETTINGS_AT, resp.server_time);
          }
        }
      }
      setPendingCount(await queueSize());
      await refreshDeadLetterCount();
      if (!sawError) {
        setStatus(isSyncActive() ? "idle" : "disabled");
        setLastError(null);
      }
      return !sawError && isSyncActive();
    } catch (error) {
      setStatus("error");
      setLastError(String(error));
      return false;
    } finally {
      flushingRef.current = false;
    }
  }, [auth.status, getDeviceId, refreshDeadLetterCount]);

  const enqueueAndTry = useCallback(
    async (op: SyncOperation) => {
      if (!enabled) return;
      await enqueue(op);
      setPendingCount(await queueSize());
      void flushQueue();
    },
    [enabled, flushQueue]
  );

  const pullAndApply = useCallback(async (opts?: { forceFull?: boolean }): Promise<void> => {
    // `forceFull` is used by enableSync: it bypasses the stale `enabled` closure
    // (enableSync sets enabled=true but this callback still captured false) AND
    // ignores any residual cursor, so activating sync always does a COMPLETE
    // reconcile instead of an incremental pull that would miss older notes.
    if ((!enabled && !opts?.forceFull) || !isSyncActive() || auth.status !== "signed-in") return;
    // One application/purge pipeline at a time: an older in-flight response
    // must not arrive after a later response has purged its local tombstone.
    if (pullingRef.current) { pullAgainRef.current = true; return; }
    pullingRef.current = true;
    setStatus("syncing");
    try {
      const session = await getQueueSession();
      await drainLocalOutbox(session.profile_id);
      const protectedKeys = await protectedEntityKeys();
      let deferred = false;
      const since = opts?.forceFull || protectedKeys.size > 0
        ? null
        : await getMeta<string | null>(KEY_LAST_PULL_AT, null);
      const cloudProfileId = await getMeta<string | null>(KEY_CLOUD_PROFILE_ID, null);
      if (!cloudProfileId) {
        // No cloud partition associated with this profile yet: nothing to pull.
        setStatus("idle");
        return;
      }
      const result = await pullAll(since, cloudProfileId);
      if (!isSyncActive()) return;

      // Settings : LWW → écrire la diff syncable dans le SettingsContext
      if (result.settings) {
        const lastPushedAt = await getMeta<string | null>(
          KEY_LAST_PUSHED_SETTINGS_AT,
          null
        );
        const merged = mergeSettingsLWW(
          settingsRef.current,
          lastPushedAt,
          result.settings
        );
        // A queued local settings push means the user just edited settings on
        // this device — don't let the pulled (stale) cloud value revert it.
        const pendingOps = await peekAll();
        const settingsPushPending = pendingOps.some(
          (e) => e.operation.kind === "settings-upsert"
        );
        if (shouldApplyCloudSettings(merged.action, settingsPushPending)) {
          await updateSettings({
            theme: merged.settings.theme,
            ui_language: merged.settings.ui_language,
            record_hotkey: merged.settings.record_hotkey,
            ptt_hotkey: merged.settings.ptt_hotkey,
            open_window_hotkey: merged.settings.open_window_hotkey,
            insertion_mode: merged.settings.insertion_mode,
            enable_sounds: merged.settings.enable_sounds,
            transcription_provider: merged.settings.transcription_provider,
            local_model_size: merged.settings.local_model_size,
          });
          await setMeta(KEY_LAST_PUSHED_SETTINGS_AT, result.settings.updated_at);
        }
      }

      // Dico
      for (const row of result.dictionary) {
        await applyRemoteWord({
          word: row.word,
          deleted: row.deleted_at !== null,
          updated_at: row.updated_at,
        });
      }

      // Snippets
      for (const row of result.snippets) {
        await applyRemoteSnippet({
          id: row.id,
          label: row.label,
          content: row.content,
          shortcut: row.shortcut,
          created_at: row.created_at,
          updated_at: row.updated_at,
          deleted_at: row.deleted_at,
        });
      }

      // Parent folders must exist before notes are imported on a fresh profile.
      // Retain per-row isolation so a corrupt item cannot abort the whole pull.
      await applyContentInDependencyOrder(
        result.folders, result.notes,
        async row => { if (!isSyncActive() || protectedKeys.has(`folder:${row.id}`)) { deferred = true; return; } const result = await applyRemoteFolder(row); deferred ||= result.deferred; },
        async row => { if (!isSyncActive() || protectedKeys.has(`note:${row.id}`)) { deferred = true; return; } const result = await applyRemoteNote(row); deferred ||= result.deferred; },
        (kind, id, error) => { deferred = true; flog(`[sync] applyRemote ${kind} failed for ${id}: ${String(error)}`, "warn"); },
        async (notes) => normalizePulledNoteFolders(result.folders, notes, new Set((await listFolders()).map((folder) => folder.id))),
      );

      // Hard-purge local tombstones the server has also tombstoned.
      // The server purges tombstones >30d via cron (sub-épique 03 Task 7).
      // We mirror that on the client by deleting the local on-disk artifacts
      // for ids the server confirms are tombstoned. The Rust commands only
      // act on entries that ARE locally soft-deleted, so passing all
      // server-tombstoned ids regardless of age is safe and convergent.
      if (!isSyncActive()) return;
      const purgeDeferred = await invoke<boolean>("purge_sync_tombstones", {
        session,
        noteIds: result.notes.filter(n => n.deleted_at !== null && !protectedKeys.has(`note:${n.id}`)).map(n => n.id),
        folderIds: result.folders.filter(f => f.deleted_at !== null && !protectedKeys.has(`folder:${f.id}`)).map(f => f.id),
      });
      deferred ||= purgeDeferred;

      // Cursor for the next incremental pull MUST come from server `updated_at`
      // values, never the client wall-clock — a client clock ahead of the server
      // would push the cursor past rows written on another device and hide them
      // permanently. Display timestamps below stay wall-clock (cosmetic only).
      const nextCursor = deferred ? null : computeNextPullCursor(since, result);
      await setMeta(KEY_LAST_PULL_AT, nextCursor);
      const nowIso = new Date().toISOString();
      setLastPullAt(nowIso);
      setLastSyncAt(nowIso);
      const unresolved = [...await peekAll(), ...await getDeadLetters()];
      const failed = unresolved.find(entry => entry.last_error !== null);
      if (failed) { setStatus(current => current === "quota-exceeded" ? current : "error"); setLastError(failed.last_error); }
      else if (unresolved.length === 0) { setStatus("idle"); setLastError(null); }
    } catch (e: unknown) {
      setStatus("error");
      setLastError(e instanceof Error ? e.message : String(e));
    } finally {
      pullingRef.current = false;
      if (pullAgainRef.current) { pullAgainRef.current = false; void pullAndApply({ forceFull: true }); }
    }
  }, [enabled, auth.status, updateSettings]);

  /** Push preferences and seed legacy content once. Note/folder payloads always
   * come from the current-state native outbox and use confirmed queue phases. */
  const fullPush = useCallback(async (): Promise<boolean> => {
    // Prevent a concurrent second full push (e.g. activation + mount retry
    // firing together). Idempotent upserts make a double push harmless, but
    // serializing avoids redundant work and interleaved status flips.
    if (pushingRef.current) return false;
    pushingRef.current = true;
    setStatus("syncing");
    try {
      const session = await getQueueSession();
      const { id: cloudProfileId, name: cloudProfileName } =
        await ensureCloudProfileId(getMeta, setMeta, session.profile_id);
      const deviceId = await getDeviceId();
      const ops: SyncOperation[] = [];
      ops.push({
        kind: "profile-upsert",
        profile: {
          id: cloudProfileId,
          name: cloudProfileName,
          updated_at: new Date().toISOString(),
          deleted_at: null,
        },
      });
      ops.push({
        kind: "settings-upsert",
        data: extractCloudSettings(settingsRef.current),
      });
      const d = await loadDictionary();
      for (const w of d.words) ops.push({ kind: "dictionary-upsert", word: w });
      const sn = await loadSnippets();
      for (const s of sn) {
        if (s.deleted_at) continue;
        ops.push({
          kind: "snippet-upsert",
          snippet: {
            id: s.id,
            label: s.label,
            content: s.content,
            shortcut: s.shortcut,
            updated_at: s.updated_at,
            deleted_at: null,
            created_at: s.created_at,
          },
        });
      }

      await invoke("seed_local_sync_snapshot", { session });
      await drainLocalOutbox(session.profile_id);
      setOversizedNoteCount(await scanOversizedNoteCount());

      let pushFailed: string | null = null;
      let quotaExceeded = false;
      if (ops.length > 0) {
        // Confirm the profile alone, then honor the existing 200-op API cap.
        const CHUNK = 200;
        let lastServerTime: string | undefined;
        for (let i = 0; i < ops.length;) {
          if (!isSyncActive()) return false;
          const chunk = i === 0 ? ops.slice(0, 1) : ops.slice(i, i + CHUNK);
          const resp = await pushOperations(chunk, deviceId, cloudProfileId);
          if (!resp.ok) {
            quotaExceeded = resp.status === 413;
            pushFailed = resp.error ?? "unknown";
            flog(
              `[sync] full push chunk ${Math.floor(i / CHUNK)} failed: ${pushFailed}`,
              "warn"
            );
            break;
          }
          // A 200 response can still carry per-op failures (FK, RLS, …). Surface
          // them instead of reporting a clean sync the DB never received.
          const firstFailed = resp.results.find((r) => !r.ok);
          if (firstFailed) {
            pushFailed = firstFailed.error ?? "operation rejected";
            flog(
              `[sync] full push op ${firstFailed.index} rejected: ${pushFailed}`,
              "warn"
            );
            break;
          }
          if (resp.server_time) lastServerTime = resp.server_time;
          i += chunk.length;
        }
        if (lastServerTime) {
          await setMeta(KEY_LAST_PUSHED_SETTINGS_AT, lastServerTime);
        }
      }
      if (pushFailed) {
        setStatus(quotaExceeded ? "quota-exceeded" : "error");
        setLastError(pushFailed);
        return false;
      }
      if (!await flushQueue()) return false;
      if (await queueSize() > 0 || (await getDeadLetters()).length > 0) return false;
      await setMeta(KEY_INITIAL_PUSH_DONE, true);
      setStatus("idle");
      setLastError(null);
      setLastSyncAt(new Date().toISOString());
      return true;
    } catch (e) {
      flog(`[sync] full push failed: ${String(e)}`, "warn");
      setStatus("error");
      setLastError(e instanceof Error ? e.message : String(e));
      return false;
    } finally {
      pushingRef.current = false;
    }
  }, [getDeviceId, flushQueue]);

  const enableSync = useCallback(async () => {
    await setMeta(KEY_ENABLED, true);
    // A fresh activation must run (or re-run) the initial full push.
    await setMeta(KEY_INITIAL_PUSH_DONE, false);
    setEnabled(true);
    setSyncActive(true);
    setStatus("idle");

    // Generate/persist the cloud_profile_id BEFORE the pull — pullAndApply
    // early-returns when the active profile has no cloud partition yet, so the
    // forceFull reconcile would be a no-op otherwise. fullPush re-ensures it
    // (idempotent: returns the same stored id).
    await ensureCloudProfileId(getMeta, setMeta, (await getQueueSession()).profile_id);

    // Legacy migration : importer settings.snippets / settings.dictionary si présents
    try {
      const current = settingsRef.current;
      await migrateLegacySnippetsOnce(current.snippets ?? []);
      await migrateLegacyDictionaryOnce(current.dictionary ?? []);
    } catch (e) {
      flog(`[sync] legacy migration failed: ${String(e)}`, "warn");
    }

    await fullPush();
    await pullAndApply({ forceFull: true });
  }, [pullAndApply, fullPush]);

  const disableSync = useCallback(async () => {
    setSyncActive(false);
    // Cancel pending wakeups; their actual intentions already live in Rust.
    try {
      await flushPendingNoteUpdates();
    } catch (e) {
      flog(`[sync] flushPendingNoteUpdates failed on disable: ${String(e)}`, "warn");
    }
    await setMeta(KEY_ENABLED, false);
    // Clear the incremental cursor so a later re-enable always full-reconciles
    // (re-activation must never resume an incremental pull from a stale cursor).
    await setMeta(KEY_LAST_PULL_AT, null);
    // Force the next activation to redo the initial full push.
    await setMeta(KEY_INITIAL_PUSH_DONE, false);
    setLastPullAt(null);
    setEnabled(false);
    setSyncActive(false);
    setStatus("disabled");
  }, []);

  const syncNow = useCallback(async () => {
    if (!enabled) return;
    await flushQueue();
    // Retry initial preferences/legacy seeding, then reconcile deferred rows.
    await fullPush();
    await pullAndApply({ forceFull: true });
  }, [enabled, flushQueue, fullPush, pullAndApply]);

  const notifySettingsChanged = useCallback(
    (previous: AppSettings["settings"], current: AppSettings["settings"]) => {
      if (!enabled) return;
      if (!syncableSettingsChanged(previous, current)) return;
      if (debounceRef.current) window.clearTimeout(debounceRef.current);
      debounceRef.current = window.setTimeout(() => {
        void enqueueAndTry({
          kind: "settings-upsert",
          data: extractCloudSettings(current),
        });
      }, DEBOUNCE_PUSH_MS);
    },
    [enabled, enqueueAndTry]
  );

  const notifyDictionaryUpserted = useCallback(
    (word: string) => {
      void enqueueAndTry({ kind: "dictionary-upsert", word });
    },
    [enqueueAndTry]
  );

  const notifyDictionaryDeleted = useCallback(
    (word: string) => {
      void enqueueAndTry({ kind: "dictionary-delete", word });
    },
    [enqueueAndTry]
  );

  const notifySnippetUpserted = useCallback(
    (id: string, label: string, content: string, shortcut: string | null) => {
      void enqueueAndTry({
        kind: "snippet-upsert",
        snippet: {
          id,
          label,
          content,
          shortcut,
          updated_at: new Date().toISOString(),
          deleted_at: null,
          created_at: new Date().toISOString(),
        },
      });
    },
    [enqueueAndTry]
  );

  const notifySnippetDeleted = useCallback(
    (id: string) => {
      void enqueueAndTry({ kind: "snippet-delete", id });
    },
    [enqueueAndTry]
  );

  // Lifecycle : login → pullAndApply + flush, then finish any initial push that
  // never completed (offline last time, transient error, oversized note now
  // trimmed). The flag makes the push a no-op once the first full push
  // succeeded. Guarded to run ONCE per signed-in session — without the ref it
  // would re-fire on every render where `pullAndApply`/`fullPush` change
  // identity, re-pulling and re-pushing needlessly.
  const mountSyncRanRef = useRef(false);
  useEffect(() => {
    if (!enabled || auth.status !== "signed-in") {
      mountSyncRanRef.current = false;
      return;
    }
    if (mountSyncRanRef.current) return;
    mountSyncRanRef.current = true;
    void (async () => {
      await flushQueue();
      await pullAndApply();
      const done = await getMeta<boolean>(KEY_INITIAL_PUSH_DONE, false);
      if (!done) await fullPush();
    })();
  }, [auth.status, enabled, pullAndApply, flushQueue, fullPush]);

  // Recompute the oversized-note count once per session when sync is on, so the
  // "too large to sync" warning survives an app restart (enableSync only runs at
  // activation time). Best-effort: failures leave the count at 0.
  useEffect(() => {
    if (!enabled) return;
    if (auth.status !== "signed-in") return;
    void scanOversizedNoteCount()
      .then(setOversizedNoteCount)
      .catch(() => {});
  }, [auth.status, enabled]);

  // Schedule a retry when the head's next_retry_at is past
  useEffect(() => {
    if (!enabled || auth.status !== "signed-in") return;
    const id = setInterval(async () => {
      if (flushingRef.current) return;
      const session = await getQueueSession();
      await drainLocalOutbox(session.profile_id);
      const ready = await peekReadyHead();
      if (ready) { await flushQueue(); await pullAndApply(); }
    }, 5_000);
    return () => clearInterval(id);
  }, [auth.status, enabled, flushQueue, pullAndApply]);

  useEffect(() => {
    if (!enabled || auth.status !== "signed-in") return;
    const wake = () => { void (async () => { await flushQueue(); await pullAndApply(); })(); };
    window.addEventListener("local-outbox-ready", wake);
    let disposed = false;
    const stops: (() => void)[] = [];
    for (const event of ["note-service-changed", "folder-service-changed"]) {
      void listen<{ profile_id: string; source: string }>(event, ({ payload }) => {
        if (payload.source === "remote") return;
        void getQueueSession().then(session => { if (!disposed && payload.profile_id === session.profile_id) wake(); });
      }).then(stop => { if (disposed) stop(); else stops.push(stop); });
    }
    return () => { disposed = true; window.removeEventListener("local-outbox-ready", wake); stops.forEach(stop => stop()); };
  }, [auth.status, enabled, flushQueue, pullAndApply]);

  // Subscribe to settings changes when sync is on so edits anywhere in the UI
  // hit `notifySettingsChanged` and trigger the debounced push.
  useEffect(() => {
    if (!enabled) return;
    const unsub = subscribeToChanges((prev, next) => {
      notifySettingsChanged(prev, next);
    });
    return unsub;
  }, [enabled, subscribeToChanges, notifySettingsChanged]);

  // Lifecycle : focus window après idle ≥ 5 min → incremental pull
  useEffect(() => {
    if (!enabled) return;
    const win = getCurrentWindow();
    let idleSince = Date.now();
    let focusUnlisten: (() => void) | undefined;

    (async () => {
      focusUnlisten = await win.onFocusChanged(({ payload: focused }) => {
        if (focused) {
          const idle = Date.now() - idleSince;
          if (idle >= FOCUS_PULL_IDLE_MS) {
            void pullAndApply();
          }
        } else {
          idleSince = Date.now();
        }
      });
    })();

    return () => {
      focusUnlisten?.();
    };
  }, [enabled, pullAndApply]);

  // Logout → disable (clears sync state but keeps local data)
  useEffect(() => {
    if (auth.status === "signed-out" && enabled) {
      void disableSync();
    }
  }, [auth.status, enabled, disableSync]);

  const value = useMemo<SyncContextValue>(
    () => ({
      enabled,
      status,
      last_sync_at: lastSyncAt,
      last_pull_at: lastPullAt,
      pending_count: pendingCount,
      dead_letter_count: deadLetterCount,
      oversized_note_count: oversizedNoteCount,
      last_error: lastError,
      enableSync,
      disableSync,
      syncNow,
      refreshDeadLetters: refreshDeadLetterCount,
      notifySettingsChanged,
      notifyDictionaryUpserted,
      notifyDictionaryDeleted,
      notifySnippetUpserted,
      notifySnippetDeleted,
    }),
    [
      enabled,
      status,
      lastSyncAt,
      lastPullAt,
      pendingCount,
      deadLetterCount,
      oversizedNoteCount,
      lastError,
      enableSync,
      disableSync,
      syncNow,
      refreshDeadLetterCount,
      notifySettingsChanged,
      notifyDictionaryUpserted,
      notifyDictionaryDeleted,
      notifySnippetUpserted,
      notifySnippetDeleted,
    ]
  );

  return <SyncContext.Provider value={value}>{children}</SyncContext.Provider>;
}
