import { describe, it, expect, vi, beforeEach } from "vitest";
import { checkQuotaForTranscription, QuotaExhausted } from "./usage";
import { _resetSupabaseClientForTest } from "./supabase";

const ENV = {
  SUPABASE_URL: "https://test.supabase.co",
  SUPABASE_SECRET_KEY: "sb_secret_test",
  GROQ_API_KEY: "groq_test",
  OPENAI_API_KEY: "openai_test",
} as const;

// Argument-blind by design: .eq(column, value) calls are accepted but their
// arguments are not asserted. This keeps the mock readable for the priority
// logic under test (which doesn't depend on filter columns being correct).
// If you add tests that DO need to validate filter arguments — e.g. ensuring
// a refactor doesn't drop the year_month filter on usage_summary — switch to
// vi.fn() per .eq() and assert the call arguments explicitly.
/**
 * Build a chainable Supabase query mock that resolves to {data, error}.
 * Each call to .from(table).select(...).eq(...)[.eq(...)...].maybeSingle()
 * resolves to the entry keyed by `table` in `returns`.
 *
 * The real Supabase client returns the same builder object from every
 * .eq() call (flat chaining), so we build a single builder object that
 * exposes both .eq() and .maybeSingle() at every level.
 */
function mockSupabaseChain(
  returns: Record<string, { data: unknown; error: unknown }>,
) {
  const makeBuilder = (table: string) => {
    const builder: {
      eq: () => typeof builder;
      maybeSingle: () => Promise<{ data: unknown; error: unknown }>;
    } = {
      eq: () => builder,
      maybeSingle: async () => returns[table] ?? { data: null, error: null },
    };
    return builder;
  };

  return {
    from: (table: string) => ({
      select: () => makeBuilder(table),
    }),
  };
}

vi.mock("@supabase/supabase-js", () => ({
  createClient: vi.fn(),
}));
import { createClient } from "@supabase/supabase-js";

beforeEach(() => {
  _resetSupabaseClientForTest();
  vi.resetAllMocks();
});

describe("checkQuotaForTranscription priority", () => {
  it("does not charge previous complimentary minutes to a paid subscription after grant revocation", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(mockSupabaseChain({
      cloud_access_grants: { data: { revoked_at: "2026-01-01T00:00:00Z" }, error: null },
      trial_status: { data: { is_active: false, minutes_remaining: 0 }, error: null },
      subscriptions: { data: { status: "active", plan: "starter", quota_minutes: 400 }, error: null },
      usage_summary: { data: { units_total: 1050, complimentary_units_total: 1000 }, error: null },
    }));
    await expect(checkQuotaForTranscription(ENV, "u1")).resolves.toEqual({
      source: "quota", remaining_minutes_estimate: 350,
    });
  });

  it("prefers trial when active with minutes remaining", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: {
          data: { is_active: true, minutes_remaining: 10 },
          error: null,
        },
        subscriptions: {
          data: { status: "active", plan: "starter", quota_minutes: 1000 },
          error: null,
        },
      }),
    );
    const result = await checkQuotaForTranscription(ENV, "u1");
    expect(result.source).toBe("trial");
    expect(result.remaining_minutes_estimate).toBe(10);
  });

  it("falls back to quota when trial is inactive", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: {
          data: { is_active: false, minutes_remaining: 0 },
          error: null,
        },
        subscriptions: {
          data: { status: "active", plan: "starter", quota_minutes: 1000 },
          error: null,
        },
        usage_summary: { data: { units_total: 200 }, error: null },
      }),
    );
    const result = await checkQuotaForTranscription(ENV, "u1");
    expect(result.source).toBe("quota");
    expect(result.remaining_minutes_estimate).toBe(800);
  });

  it("falls back to overage when quota is exhausted", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: {
          data: { is_active: false, minutes_remaining: 0 },
          error: null,
        },
        subscriptions: {
          data: { status: "active", plan: "starter", quota_minutes: 1000 },
          error: null,
        },
        usage_summary: { data: { units_total: 1100 }, error: null },
      }),
    );
    const result = await checkQuotaForTranscription(ENV, "u1");
    expect(result.source).toBe("overage");
    // overage allowance = 300, used overage = 100 → 200 left
    expect(result.remaining_minutes_estimate).toBe(200);
  });

  it("denies when no active subscription and no trial", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: {
          data: { is_active: false, minutes_remaining: 0 },
          error: null,
        },
        subscriptions: { data: null, error: null },
      }),
    );
    const err = await checkQuotaForTranscription(ENV, "u1").catch((e) => e);
    expect(err).toBeInstanceOf(QuotaExhausted);
    expect((err as QuotaExhausted).reason).toBe("no_active_subscription");
  });

  it("denies when subscription expired even if quota would allow", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: {
          data: { is_active: false, minutes_remaining: 0 },
          error: null,
        },
        subscriptions: {
          data: { status: "expired", plan: "starter", quota_minutes: 1000 },
          error: null,
        },
        usage_summary: { data: { units_total: 0 }, error: null },
      }),
    );
    const err = await checkQuotaForTranscription(ENV, "u1").catch((e) => e);
    expect(err).toBeInstanceOf(QuotaExhausted);
    expect((err as QuotaExhausted).reason).toBe("no_active_subscription");
  });

  it("denies when overage hard cap is reached", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: {
          data: { is_active: false, minutes_remaining: 0 },
          error: null,
        },
        subscriptions: {
          data: { status: "active", plan: "starter", quota_minutes: 1000 },
          error: null,
        },
        usage_summary: { data: { units_total: 1400 }, error: null }, // 400 over the 300 cap
      }),
    );
    const err = await checkQuotaForTranscription(ENV, "u1").catch((e) => e);
    expect(err).toBeInstanceOf(QuotaExhausted);
    expect((err as QuotaExhausted).reason).toBe("hard_cap_reached");
  });

  it("propagates trial_status fetch errors", async () => {
    (createClient as ReturnType<typeof vi.fn>).mockReturnValue(
      mockSupabaseChain({
        trial_status: { data: null, error: { message: "db unavailable" } },
      }),
    );
    await expect(checkQuotaForTranscription(ENV, "u1")).rejects.toThrow(
      /trial_status fetch failed: db unavailable/,
    );
  });
});
