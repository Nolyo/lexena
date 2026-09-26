import { beforeEach, describe, expect, it, vi } from "vitest";
import { checkQuotaForTranscription, QuotaExhausted } from "./usage";
import { handlePostProcess } from "./post-process";
import { handleNotesAssist } from "./notes-assist";
import { _resetSupabaseClientForTest } from "./supabase";

vi.mock("@supabase/supabase-js", () => ({ createClient: vi.fn() }));
vi.mock("./openai", async (original) => ({
  ...await original<typeof import("./openai")>(),
  chatCompletion: vi.fn(async () => ({ text: "Done", tokens_in: 10, tokens_out: 5, model: "test" })),
}));
import { createClient } from "@supabase/supabase-js";
import { chatCompletion } from "./openai";

const ENV = { SUPABASE_URL: "https://test.supabase.co", SUPABASE_SECRET_KEY: "test", GROQ_API_KEY: "g", OPENAI_API_KEY: "o" };
const activeGrant = {
  user_id: "owner", revoked_at: null, expires_at: null,
  monthly_minutes_limit: 1000, monthly_tokens_limit: 1_000_000,
};
let grant: typeof activeGrant | Record<string, unknown> | null;
let lookupError: { message: string } | null;
let usageError: { message: string } | null;
let minutes: number;
let tokens: number;
let recorded: Record<string, unknown>[];

beforeEach(() => {
  _resetSupabaseClientForTest();
  vi.clearAllMocks();
  grant = { ...activeGrant };
  lookupError = null;
  usageError = null;
  minutes = 25;
  tokens = 100;
  recorded = [];
  (createClient as ReturnType<typeof vi.fn>).mockReturnValue({
    from(table: string) {
      const filters: Record<string, unknown> = {};
      const builder = {
        select: () => builder,
        eq: (key: string, value: unknown) => { filters[key] = value; return builder; },
        insert: (event: Record<string, unknown>) => { recorded.push(event); return builder; },
        single: async () => ({ data: { id: "event" }, error: null }),
        maybeSingle: async () => {
          if (table === "cloud_access_grants") return {
            data: filters.user_id === grant?.user_id ? grant : null, error: lookupError,
          };
          if (table === "usage_summary") {
            const now = new Date();
            const month = `${now.getUTCFullYear()}-${String(now.getUTCMonth() + 1).padStart(2, "0")}`;
            if (!filters.user_id || filters.year_month !== month) throw new Error("unscoped usage lookup");
            if (filters.user_id !== "owner") return { data: null, error: null };
            return { data: { units_total: filters.kind === "transcription" ? minutes : tokens }, error: usageError };
          }
          return { data: null, error: null }; // expired trial and no subscription
        },
      };
      return builder;
    },
  });
});

describe("server-managed owner cloud access", () => {
  it("allows the granted account after trial/subscription expiry, as complimentary usage", async () => {
    await expect(checkQuotaForTranscription(ENV, "owner")).resolves.toEqual({
      source: "complimentary", remaining_minutes_estimate: 975,
    });
  });

  it("does not give another authenticated account the owner's access", async () => {
    await expect(checkQuotaForTranscription(ENV, "attacker")).rejects.toBeInstanceOf(QuotaExhausted);
  });

  it.each([
    { revoked_at: "2020-01-01T00:00:00Z" },
    { expires_at: "2020-01-01T00:00:00Z" },
    { expires_at: "invalid" },
  ])("refuses a revoked/expired/malformed grant: %j", async (change) => {
    grant = { ...activeGrant, ...change };
    await expect(checkQuotaForTranscription(ENV, "owner")).rejects.toBeInstanceOf(QuotaExhausted);
  });

  it("observes revocation on the next request without waiting for a JWT refresh", async () => {
    await expect(checkQuotaForTranscription(ENV, "owner")).resolves.toHaveProperty("source", "complimentary");
    grant = { ...activeGrant, revoked_at: new Date().toISOString() };
    await expect(checkQuotaForTranscription(ENV, "owner")).rejects.toBeInstanceOf(QuotaExhausted);
  });

  it("refuses to assume access when the grants query fails", async () => {
    lookupError = { message: "database unavailable" };
    await expect(checkQuotaForTranscription(ENV, "owner")).rejects.toThrow(/grant.*fetch failed/);
  });

  it("refuses to assume zero usage when the counter query fails", async () => {
    usageError = { message: "database unavailable" };
    await expect(checkQuotaForTranscription(ENV, "owner")).rejects.toThrow(/usage.*fetch failed/);
  });

  it("stops at the owner minute limit without paid overage", async () => {
    minutes = 1000;
    await expect(checkQuotaForTranscription(ENV, "owner")).rejects.toMatchObject({ reason: "hard_cap_reached" });
  });

  for (const [path, handle, body] of [
    ["post-process", handlePostProcess, { task: "auto", text: "Bonjour" }],
    ["notes-assist", handleNotesAssist, { system_prompt: "Improve", user_text: "Bonjour" }],
  ] as const) {
    const request = () => new Request(`https://api.test/${path}`, { method: "POST", body: JSON.stringify(body) });
    it(`${path} allows the owner and records non-billable token usage`, async () => {
      const res = await handle(request(), ENV, { user_id: "owner" });
      expect(res.status).toBe(200);
      expect(await res.json()).toMatchObject({ text: "Done", source: "complimentary" });
      expect(recorded).toEqual([expect.objectContaining({ user_id: "owner", source: "complimentary", units: 15, kind: "post_process" })]);
    });
    it(`${path} refuses an exhausted token allowance before calling OpenAI`, async () => {
      tokens = 1_000_000;
      const res = await handle(request(), ENV, { user_id: "owner" });
      expect(res.status).toBe(402);
      expect(chatCompletion).not.toHaveBeenCalled();
    });
    it(`${path} ignores self-assigned admin metadata`, async () => {
      const res = await handle(request(), ENV, { user_id: "attacker", ...{ user_metadata: { role: "admin" } } });
      expect(res.status).toBe(402);
      expect(chatCompletion).not.toHaveBeenCalled();
    });
  }
});
