import { createContext, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { supabase } from "@/lib/supabase";
import { useAuth } from "@/hooks/useAuth";
import { useSettings } from "@/hooks/useSettings";
import type { MonthlyBreakdown } from "@/lib/usage/breakdown";
import { computeBreakdown } from "@/lib/usage/breakdown";
import { activeCloudAccessGrant, type CloudAccessGrant } from "@/lib/cloud/access";

export type CloudMode = "local" | "cloud" | "uninitialized";

export interface TrialStatus {
  is_active: boolean;
  minutes_remaining: number;
  expires_at: string | null;
}

export interface UsagePlan {
  quota_minutes: number;
  plan: "starter" | "pro";
}

export interface CloudContextValue {
  /**
   * Effective routing for the next transcription / post-process call.
   * "cloud" requires: signed-in user, server-side eligibility (active trial
   * or active subscription or server-managed grant), AND the user explicitly picked "LexenaCloud" as
   * their transcription provider in settings. Anything else falls back to
   * "local" — meaning the local Whisper / user's API key path.
   */
  mode: CloudMode;
  isCloudEligible: boolean;
  hasCloudSelected: boolean;

  // Usage data, hoisted here so QuotaCounter and CloudSection share a single
  // fetch instead of each mounting their own copy of useUsage.
  trial: TrialStatus;
  monthly_minutes_used: number;
  monthly_minutes_breakdown: MonthlyBreakdown;
  plan: UsagePlan | null;
  ownerAccess: CloudAccessGrant | null;
  usageLoading: boolean;
  refreshUsage: () => Promise<void>;
}

const DEFAULT_TRIAL: TrialStatus = {
  is_active: false,
  minutes_remaining: 0,
  expires_at: null,
};

const DEFAULT_BREAKDOWN: MonthlyBreakdown = { trial: 0, quota: 0, overage: 0 };

export const CloudContext = createContext<CloudContextValue>({
  mode: "uninitialized",
  isCloudEligible: false,
  hasCloudSelected: false,
  trial: DEFAULT_TRIAL,
  monthly_minutes_used: 0,
  monthly_minutes_breakdown: DEFAULT_BREAKDOWN,
  plan: null,
  ownerAccess: null,
  usageLoading: false,
  refreshUsage: async () => {},
});

function currentYearMonth(): string {
  const d = new Date();
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}`;
}

function currentMonthBoundsUtc(): { start: string; end: string } {
  const d = new Date();
  const start = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1));
  const end = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1));
  return { start: start.toISOString(), end: end.toISOString() };
}

export function CloudProvider({ children }: { children: ReactNode }) {
  const { user } = useAuth();
  const { settings: { transcription_provider, streaming_mode } } = useSettings();

  const [snapshotEligible, setEligible] = useState(false);
  const [ownerGrant, setOwnerGrant] = useState<CloudAccessGrant | null>(null);
  const [loadedUserId, setLoadedUserId] = useState<string | null>(null);
  const activeUserId = useRef(user?.id);
  activeUserId.current = user?.id;
  const requestId = useRef(0);
  const [trial, setTrial] = useState<TrialStatus>(DEFAULT_TRIAL);
  const [monthlyUsed, setMonthlyUsed] = useState(0);
  const [monthlyBreakdown, setMonthlyBreakdown] =
    useState<MonthlyBreakdown>(DEFAULT_BREAKDOWN);
  const [plan, setPlan] = useState<UsagePlan | null>(null);
  const [usageLoading, setUsageLoading] = useState(false);

  const hasCloudSelected = transcription_provider === "LexenaCloud";
  const currentSnapshot = Boolean(user && loadedUserId === user.id);
  const eligible = currentSnapshot && snapshotEligible;
  const ownerAccess = currentSnapshot ? ownerGrant : null;

  const refreshUsage = useCallback(async () => {
    const request = ++requestId.current;
    const isCurrent = () => request === requestId.current && activeUserId.current === user?.id;
    if (!user) {
      setTrial(DEFAULT_TRIAL);
      setMonthlyUsed(0);
      setMonthlyBreakdown(DEFAULT_BREAKDOWN);
      setPlan(null);
      setOwnerGrant(null);
      setLoadedUserId(null);
      setEligible(false);
      setUsageLoading(false);
      return;
    }
    setUsageLoading(true);
    try {
      const ym = currentYearMonth();
      const { start, end } = currentMonthBoundsUtc();
      const [
        { data: trialData, error: trialError },
        { data: usage },
        { data: sub, error: subError },
        { data: events },
        { data: grantData, error: grantError },
      ] = await Promise.all([
        supabase.from("trial_status").select("*").eq("user_id", user.id).maybeSingle(),
        supabase
          .from("usage_summary")
          .select("units_total, complimentary_units_total")
          .eq("user_id", user.id)
          .eq("year_month", ym)
          .eq("kind", "transcription")
          .maybeSingle(),
        supabase
          .from("subscriptions")
          .select("plan, quota_minutes, status")
          .eq("user_id", user.id)
          .maybeSingle(),
        supabase
          .from("usage_events")
          .select("source, units")
          .eq("user_id", user.id)
          .eq("kind", "transcription")
          .gte("created_at", start)
          .lt("created_at", end),
        supabase.from("cloud_access_grants")
          .select("monthly_minutes_limit, monthly_tokens_limit, expires_at, revoked_at")
          .eq("user_id", user.id).maybeSingle(),
      ]);
      if (!isCurrent()) return;
      // Never carry a previous grant forward after a failed authorization read.
      const grant = grantError ? null : activeCloudAccessGrant(grantData);

      const t: TrialStatus = {
        is_active: !trialError && Boolean(trialData?.is_active),
        minutes_remaining: Number(trialData?.minutes_remaining ?? 0),
        expires_at: (trialData?.expires_at as string) ?? null,
      };
      setTrial(t);
      const total = Number(usage?.units_total ?? 0);
      setMonthlyUsed(grant ? total : Math.max(0, total - Number(usage?.complimentary_units_total ?? 0)));
      setMonthlyBreakdown(computeBreakdown(events ?? []));
      setPlan(
        !subError && sub && sub.status === "active"
          ? { quota_minutes: Number(sub.quota_minutes), plan: sub.plan as "starter" | "pro" }
          : null,
      );
      setOwnerGrant(grant);
      setLoadedUserId(user.id);
      setEligible(Boolean(grant) || t.is_active || (!subError && sub?.status === "active"));
    } catch {
      if (!isCurrent()) return;
      setTrial(DEFAULT_TRIAL);
      setPlan(null);
      setOwnerGrant(null);
      setEligible(false);
      setLoadedUserId(user.id);
    } finally {
      if (isCurrent()) setUsageLoading(false);
    }
  }, [user]);

  useEffect(() => {
    void refreshUsage();
    const onFocus = () => { void refreshUsage(); };
    window.addEventListener("focus", onFocus);
    // Refresh revocations/expiry while the desktop remains open. Server checks
    // every request independently, including when this UI snapshot is stale.
    const timer = window.setInterval(onFocus, 60_000);
    return () => {
      ++requestId.current;
      window.removeEventListener("focus", onFocus);
      window.clearInterval(timer);
    };
  }, [refreshUsage]);

  // Refresh subscription/trial state when the user returns from a successful
  // Lemon Squeezy checkout. The Rust deep-link handler emits this event when
  // it receives `lexena://billing/success`. By the time the webhook has been
  // processed by Supabase, the next `refreshUsage()` should observe the new
  // `subscriptions.status = 'active'` row.
  useEffect(() => {
    const unlistenPromise = listen("billing-checkout-completed", () => {
      void refreshUsage();
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [refreshUsage]);

  // Push the cloud routing snapshot to Rust so hotkey-triggered start_recording
  // can refuse capture when LexenaCloud is selected but the user isn't eligible
  // (instead of capturing 20s of audio that the renderer would reject post-hoc).
  // The Rust side stores this in `AppState.cloud_gate` and emits
  // `cloud-gate-blocked` when a hotkey is refused — listened to in
  // useRecordingWorkflow which surfaces the i18n toast.
  useEffect(() => {
    void invoke("set_cloud_gate", {
      provider: transcription_provider,
      eligible,
    }).catch(() => {
      // Non-fatal: the renderer-side check in useRecordingWorkflow still
      // refuses the call. Worst case, hotkey path captures audio then we
      // reject in transcribeAudio (the pre-PR behavior).
    });
  }, [transcription_provider, eligible]);

  const mode: CloudMode = useMemo(() => {
    if (!user) return "local";
    if (!hasCloudSelected) return "local";
    return eligible ? "cloud" : "local";
  }, [user, eligible, hasCloudSelected]);

  // Push the streaming-mode snapshot to Rust: both recording start paths read
  // it to decide whether to open a streaming session. Effective only when the
  // user enabled the setting AND the cloud route is actually usable.
  useEffect(() => {
    const enabled = Boolean(streaming_mode) && mode === "cloud";
    void invoke("set_streaming_enabled", { enabled }).catch(() => {
      // Non-fatal: with a stale/false flag Rust simply keeps batch behavior.
    });
  }, [streaming_mode, mode]);

  const value = useMemo<CloudContextValue>(
    () => ({
      mode,
      isCloudEligible: eligible,
      hasCloudSelected,
      trial: currentSnapshot ? trial : DEFAULT_TRIAL,
      monthly_minutes_used: currentSnapshot ? monthlyUsed : 0,
      monthly_minutes_breakdown: currentSnapshot ? monthlyBreakdown : DEFAULT_BREAKDOWN,
      plan: currentSnapshot ? plan : null,
      ownerAccess,
      usageLoading: usageLoading || Boolean(user && !currentSnapshot),
      refreshUsage,
    }),
    [
      mode,
      eligible,
      hasCloudSelected,
      trial,
      monthlyUsed,
      monthlyBreakdown,
      plan,
      ownerAccess,
      currentSnapshot,
      user,
      usageLoading,
      refreshUsage,
    ],
  );
  return <CloudContext.Provider value={value}>{children}</CloudContext.Provider>;
}
