// workers/transcription-api/src/types.ts
export interface Env {
  SUPABASE_URL: string;
  SUPABASE_SECRET_KEY: string;
  GROQ_API_KEY: string;
  OPENAI_API_KEY: string;
}

export interface AuthenticatedUser {
  user_id: string;
  email?: string;
}

export type UsageKind = "transcription" | "post_process";
export type UsageUnit = "minutes" | "tokens";
export type UsageSource = "trial" | "quota" | "overage" | "complimentary";

export interface UsageEventInput {
  user_id: string;
  kind: UsageKind;
  units: number;
  units_unit: UsageUnit;
  model: string;
  provider: "groq" | "openai";
  provider_request_id?: string;
  idempotency_key?: string;
  source: UsageSource;
}

export interface QuotaContext {
  source: UsageSource;
  // Remaining minutes available (combined trial + quota + overage allowance).
  remaining_minutes_estimate: number;
}
