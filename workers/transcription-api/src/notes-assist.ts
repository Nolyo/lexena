import type { Env, AuthenticatedUser } from "./types";
import { chatCompletion, OpenAIError, type OpenAIModelTier } from "./openai";
import { recordUsageEvent, checkTextProcessingAccess, QuotaExhausted } from "./usage";
import { errorResponse } from "./errors";

const MAX_USER_TEXT_CHARS = 50_000;
const MAX_SYSTEM_PROMPT_CHARS = 5_000;

interface NotesAssistBody {
  system_prompt: string;
  user_text: string;
  model_tier?: string;
}

/**
 * Free-form AI assistant used by the Notes editor bubble menu. The caller
 * supplies its own system prompt (translate / improve / custom...). Eligibility
 * mirrors `/post-process`: server grant, active trial or subscription. Usage is
 * accounted in tokens against the `post_process` bucket — the user's billing
 * surface treats AI text processing as a single category.
 */
export async function handleNotesAssist(
  req: Request,
  env: Env,
  user: AuthenticatedUser,
): Promise<Response> {
  const idempotencyKey = req.headers.get("Idempotency-Key") ?? undefined;

  let body: NotesAssistBody;
  try {
    body = (await req.json()) as NotesAssistBody;
  } catch {
    return errorResponse("bad_request", "invalid JSON body");
  }

  if (typeof body.system_prompt !== "string" || !body.system_prompt.trim()) {
    return errorResponse("bad_request", "missing or empty 'system_prompt'");
  }
  if (typeof body.user_text !== "string" || !body.user_text.trim()) {
    return errorResponse("bad_request", "missing or empty 'user_text'");
  }
  if (body.system_prompt.length > MAX_SYSTEM_PROMPT_CHARS) {
    return errorResponse(
      "payload_too_large",
      `system_prompt too long (max ${MAX_SYSTEM_PROMPT_CHARS} chars)`,
    );
  }
  if (body.user_text.length > MAX_USER_TEXT_CHARS) {
    return errorResponse(
      "payload_too_large",
      `user_text too long (max ${MAX_USER_TEXT_CHARS} chars)`,
    );
  }

  const tier: OpenAIModelTier = body.model_tier === "full" ? "full" : "mini";

  let source;
  try {
    source = await checkTextProcessingAccess(env, user.user_id);
  } catch (err) {
    if (err instanceof QuotaExhausted) return errorResponse("quota_exhausted", err.reason);
    throw err;
  }

  let result;
  try {
    result = await chatCompletion(body.system_prompt, body.user_text, env, tier);
  } catch (err) {
    if (err instanceof OpenAIError) {
      if (err.retryable) {
        return errorResponse("provider_unavailable", `openai ${err.status}`);
      }
      return errorResponse("bad_request", `openai rejected request: ${err.status}`);
    }
    throw err;
  }

  const totalTokens = result.tokens_in + result.tokens_out;
  const { event_id } = await recordUsageEvent(env, {
    user_id: user.user_id,
    kind: "post_process",
    units: totalTokens,
    units_unit: "tokens",
    model: result.model,
    provider: "openai",
    provider_request_id: result.request_id,
    idempotency_key: idempotencyKey,
    source,
  });

  return Response.json({
    text: result.text,
    tokens_in: result.tokens_in,
    tokens_out: result.tokens_out,
    request_id: event_id,
    source,
  });
}
