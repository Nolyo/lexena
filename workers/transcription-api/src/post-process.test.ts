import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock the openai module to control chatCompletion behavior in tests.
vi.mock("./openai", async () => {
  const actual = await vi.importActual<typeof import("./openai")>("./openai");
  return {
    ...actual,
    chatCompletion: vi.fn(),
  };
});

// Mock usage to bypass eligibility/recording. We want to reach the chatCompletion
// call so the catch path runs.
vi.mock("./usage", async (original) => ({
  ...await original<typeof import("./usage")>(),
  checkTextProcessingAccess: vi.fn(async () => "trial"),
  recordUsageEvent: vi.fn(async () => ({ event_id: "evt_test", deduplicated: false })),
}));

import { handlePostProcess } from "./post-process";
import { chatCompletion, OpenAIError } from "./openai";
import type { AuthenticatedUser, Env } from "./types";

const ENV: Env = {
  SUPABASE_URL: "https://test.supabase.co",
  SUPABASE_SECRET_KEY: "sb_secret_test",
  GROQ_API_KEY: "g",
  OPENAI_API_KEY: "o",
};

const USER: AuthenticatedUser = { user_id: "user-1", email: "u@test.local" };

function makeRequest(body: Record<string, unknown>): Request {
  return new Request("https://api.test/post-process", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("handlePostProcess OpenAIError mapping", () => {
  it("maps 4xx non-retryable OpenAIError to 400 bad_request", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new OpenAIError("openai 400: content policy", 400, false),
    );
    const res = await handlePostProcess(
      makeRequest({ task: "auto", text: "Bonjour" }),
      ENV,
      USER,
    );
    expect(res.status).toBe(400);
    const body = (await res.json()) as { error: string; message: string };
    expect(body.error).toBe("bad_request");
    expect(body.message).toContain("openai rejected request: 400");
  });

  it("maps 5xx OpenAIError to 502 provider_unavailable", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new OpenAIError("openai 503: down", 503, true),
    );
    const res = await handlePostProcess(
      makeRequest({ task: "auto", text: "Bonjour" }),
      ENV,
      USER,
    );
    expect(res.status).toBe(502);
    const body = (await res.json()) as { error: string; message: string };
    expect(body.error).toBe("provider_unavailable");
    expect(body.message).toBe("openai 503");
  });

  it("maps 429 retryable OpenAIError to 502 provider_unavailable", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new OpenAIError("openai 429: rate limit", 429, true),
    );
    const res = await handlePostProcess(
      makeRequest({ task: "auto", text: "Bonjour" }),
      ENV,
      USER,
    );
    expect(res.status).toBe(502);
    const body = (await res.json()) as { error: string; message: string };
    expect(body.error).toBe("provider_unavailable");
    expect(body.message).toBe("openai 429");
  });
});

describe("handlePostProcess custom_instructions", () => {
  const OK_RESULT = {
    text: "ok",
    tokens_in: 10,
    tokens_out: 5,
    model: "gpt-4o-mini",
    request_id: "oai_req",
  };

  it("rejects non-string custom_instructions with 400 without calling OpenAI", async () => {
    const res = await handlePostProcess(
      makeRequest({ task: "auto", text: "Bonjour", custom_instructions: 42 }),
      ENV,
      USER,
    );
    expect(res.status).toBe(400);
    const body = (await res.json()) as { error: string };
    expect(body.error).toBe("bad_request");
    expect(chatCompletion).not.toHaveBeenCalled();
  });

  it("rejects custom_instructions over 1000 chars with 400 without calling OpenAI", async () => {
    const res = await handlePostProcess(
      makeRequest({
        task: "auto",
        text: "Bonjour",
        custom_instructions: "x".repeat(1001),
      }),
      ENV,
      USER,
    );
    expect(res.status).toBe(400);
    const body = (await res.json()) as { error: string; message: string };
    expect(body.error).toBe("bad_request");
    expect(body.message).toContain("custom_instructions too long");
    expect(chatCompletion).not.toHaveBeenCalled();
  });

  it("accepts exactly 1000 chars", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockResolvedValueOnce(OK_RESULT);
    const res = await handlePostProcess(
      makeRequest({
        task: "auto",
        text: "Bonjour",
        custom_instructions: "x".repeat(1000),
      }),
      ENV,
      USER,
    );
    expect(res.status).toBe(200);
  });

  it("treats null as absent — no <user_instructions> block sent", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockResolvedValueOnce(OK_RESULT);
    const res = await handlePostProcess(
      makeRequest({ task: "auto", text: "Bonjour", custom_instructions: null }),
      ENV,
      USER,
    );
    expect(res.status).toBe(200);
    const userPrompt = (chatCompletion as ReturnType<typeof vi.fn>).mock
      .calls[0][1] as string;
    expect(userPrompt).not.toContain("<user_instructions>");
    expect(userPrompt).toContain("<dictation>");
  });

  it("treats whitespace-only instructions as absent", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockResolvedValueOnce(OK_RESULT);
    await handlePostProcess(
      makeRequest({ task: "auto", text: "Bonjour", custom_instructions: "   \n " }),
      ENV,
      USER,
    );
    const userPrompt = (chatCompletion as ReturnType<typeof vi.fn>).mock
      .calls[0][1] as string;
    expect(userPrompt).not.toContain("<user_instructions>");
  });

  it("injects trimmed instructions in a <user_instructions> block before the dictation", async () => {
    (chatCompletion as ReturnType<typeof vi.fn>).mockResolvedValueOnce(OK_RESULT);
    await handlePostProcess(
      makeRequest({
        task: "auto",
        text: "j'ai poussé le secret dans volt",
        custom_instructions: "  Remplace volt par Vault.  ",
      }),
      ENV,
      USER,
    );
    const [systemPrompt, userPrompt] = (chatCompletion as ReturnType<typeof vi.fn>).mock
      .calls[0] as [string, string];
    expect(userPrompt).toBe(
      "<user_instructions>\nRemplace volt par Vault.\n</user_instructions>\n<dictation>\nj'ai poussé le secret dans volt\n</dictation>",
    );
    // Instructions must never reach the system prompt.
    expect(systemPrompt).not.toContain("Remplace volt par Vault.");
  });
});
