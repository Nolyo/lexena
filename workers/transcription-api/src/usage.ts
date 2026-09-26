import type { Env, QuotaContext, UsageEventInput, UsageKind, UsageSource } from "./types";
import { getSupabaseAdmin } from "./supabase";
import { activeCloudAccessGrant } from "../../../src/lib/cloud/access";

interface TrialStatus {
  is_active: boolean;
  minutes_remaining: number;
}

interface SubscriptionState {
  status: "active" | "paused" | "expired" | null;
  plan: "starter" | "pro" | null;
  quota_minutes: number;
  overage_minutes_allowed: number;
  current_month: string; // 'YYYY-MM' UTC
  used_minutes_this_month: number;
}

const HARD_CAP_OVERAGE_MINUTES = 300; // 5h soft cap fair use (premium spec §12.1)

function currentYearMonth(date = new Date()): string {
  const y = date.getUTCFullYear();
  const m = String(date.getUTCMonth() + 1).padStart(2, "0");
  return `${y}-${m}`;
}

export class QuotaExhausted extends Error {
  constructor(public reason: "no_active_subscription" | "hard_cap_reached") {
    super(`quota exhausted: ${reason}`);
    this.name = "QuotaExhausted";
  }
}

/** Read the grant on every request: never trust email, user metadata or UI flags. */
async function complimentaryAllowance(env: Env, user_id: string, kind: UsageKind): Promise<number | null> {
  const sb = getSupabaseAdmin(env);
  const { data, error } = await sb.from("cloud_access_grants")
    .select("monthly_minutes_limit, monthly_tokens_limit, expires_at, revoked_at")
    .eq("user_id", user_id).maybeSingle();
  if (error) throw new Error(`cloud grant fetch failed: ${error.message}`);
  const grant = activeCloudAccessGrant(data);
  if (!grant) return null;

  const { data: usage, error: usageError } = await sb.from("usage_summary")
    .select("units_total").eq("user_id", user_id).eq("kind", kind)
    .eq("year_month", currentYearMonth()).maybeSingle();
  if (usageError) throw new Error(`usage fetch failed: ${usageError.message}`);
  const used = Number(usage?.units_total ?? 0);
  if (!Number.isFinite(used) || used < 0) throw new Error("invalid usage counter");
  const limit = kind === "transcription" ? grant.monthly_minutes_limit : grant.monthly_tokens_limit;
  const remaining = limit - used;
  // No fallback to billable quota/overage when a complimentary allowance runs out.
  if (remaining <= 0) throw new QuotaExhausted("hard_cap_reached");
  return remaining;
}

/** Shared authorization for post-processing and the notes assistant. */
export async function checkTextProcessingAccess(env: Env, user_id: string): Promise<UsageSource> {
  if (await complimentaryAllowance(env, user_id, "post_process") !== null) return "complimentary";
  const [trial, sub] = await Promise.all([
    fetchTrialStatus(env, user_id), fetchSubscriptionState(env, user_id),
  ]);
  if (trial.is_active) return "trial";
  if (sub.status === "active" && sub.plan) return "quota";
  throw new QuotaExhausted("no_active_subscription");
}

export async function fetchTrialStatus(
  env: Env,
  user_id: string,
): Promise<TrialStatus> {
  const sb = getSupabaseAdmin(env);
  const { data, error } = await sb
    .from("trial_status")
    .select("is_active, minutes_remaining")
    .eq("user_id", user_id)
    .maybeSingle();
  if (error) throw new Error(`trial_status fetch failed: ${error.message}`);
  return {
    is_active: Boolean(data?.is_active),
    minutes_remaining: Number(data?.minutes_remaining ?? 0),
  };
}

export async function fetchSubscriptionState(
  env: Env,
  user_id: string,
): Promise<SubscriptionState> {
  const sb = getSupabaseAdmin(env);
  const yearMonth = currentYearMonth();

  const [{ data: sub, error: subError }, { data: usage, error: usageError }] = await Promise.all([
    sb.from("subscriptions").select("status, plan, quota_minutes").eq("user_id", user_id).maybeSingle(),
    sb.from("usage_summary")
      .select("units_total, complimentary_units_total")
      .eq("user_id", user_id)
      .eq("kind", "transcription")
      .eq("year_month", yearMonth)
      .maybeSingle(),
  ]);
  if (subError) throw new Error(`subscription fetch failed: ${subError.message}`);
  if (usageError) throw new Error(`usage fetch failed: ${usageError.message}`);

  return {
    status: (sub?.status as SubscriptionState["status"]) ?? null,
    plan: (sub?.plan as SubscriptionState["plan"]) ?? null,
    quota_minutes: Number(sub?.quota_minutes ?? 0),
    overage_minutes_allowed: HARD_CAP_OVERAGE_MINUTES,
    current_month: yearMonth,
    used_minutes_this_month: Math.max(0, Number(usage?.units_total ?? 0) - Number(usage?.complimentary_units_total ?? 0)),
  };
}

/**
 * Determine which "wallet" to debit for a transcription request.
 * Priority: server-managed grant > trial > quota > overage > deny.
 *
 * Race note: this read+later-insert is best-effort, not serialized. Two
 * concurrent transcriptions from the same user can each see "1 minute
 * remaining" and each consume 1, leaving the trial 1 minute over. The CHECK
 * constraint on trial_credits (consumed <= granted * 1.05) caps the slop at
 * 5%; beyond that the trigger raises and the second event rolls back. Cost of
 * a typical race is well under a cent of Groq spend — accepted as a tradeoff
 * vs. the latency cost of a pessimistic lock.
 */
export async function checkQuotaForTranscription(
  env: Env,
  user_id: string,
): Promise<QuotaContext> {
  const complimentary = await complimentaryAllowance(env, user_id, "transcription");
  if (complimentary !== null) {
    return { source: "complimentary", remaining_minutes_estimate: complimentary };
  }
  const trial = await fetchTrialStatus(env, user_id);
  if (trial.is_active && trial.minutes_remaining > 0) {
    return { source: "trial", remaining_minutes_estimate: trial.minutes_remaining };
  }

  const sub = await fetchSubscriptionState(env, user_id);
  if (sub.status !== "active" || !sub.plan) {
    throw new QuotaExhausted("no_active_subscription");
  }

  const remaining_quota = sub.quota_minutes - sub.used_minutes_this_month;
  if (remaining_quota > 0) {
    return { source: "quota", remaining_minutes_estimate: remaining_quota };
  }

  const used_overage = -remaining_quota;
  if (used_overage < sub.overage_minutes_allowed) {
    return {
      source: "overage",
      remaining_minutes_estimate: sub.overage_minutes_allowed - used_overage,
    };
  }

  throw new QuotaExhausted("hard_cap_reached");
}

/**
 * Insert a usage_events row + atomically debit trial_credits if source=trial.
 * Idempotent on (user_id, idempotency_key).
 */
export async function recordUsageEvent(
  env: Env,
  event: UsageEventInput,
): Promise<{ event_id: string; deduplicated: boolean }> {
  const sb = getSupabaseAdmin(env);

  if (event.idempotency_key) {
    const { data: existing } = await sb
      .from("usage_events")
      .select("id")
      .eq("user_id", event.user_id)
      .eq("idempotency_key", event.idempotency_key)
      .maybeSingle();
    if (existing) {
      return { event_id: existing.id as string, deduplicated: true };
    }
  }

  const { data, error } = await sb
    .from("usage_events")
    .insert({
      user_id: event.user_id,
      kind: event.kind,
      units: event.units,
      units_unit: event.units_unit,
      model: event.model,
      provider: event.provider,
      provider_request_id: event.provider_request_id ?? null,
      idempotency_key: event.idempotency_key ?? null,
      source: event.source,
    })
    .select("id")
    .single();

  if (error) {
    if (event.idempotency_key && (error as { code?: string }).code === "23505") {
      const { data: existing } = await sb
        .from("usage_events")
        .select("id")
        .eq("user_id", event.user_id)
        .eq("idempotency_key", event.idempotency_key)
        .single();
      return { event_id: existing!.id as string, deduplicated: true };
    }
    throw new Error(`recordUsageEvent failed: ${error.message}`);
  }

  return { event_id: data!.id as string, deduplicated: false };
}
