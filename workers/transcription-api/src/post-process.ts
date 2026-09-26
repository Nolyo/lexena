import type { Env, AuthenticatedUser } from "./types";
import { chatCompletion, OpenAIError, type OpenAIModelTier } from "./openai";
import { getPromptTemplate, isValidTask } from "./prompts";
import { recordUsageEvent, checkTextProcessingAccess, QuotaExhausted } from "./usage";
import { errorResponse } from "./errors";

const MAX_INPUT_CHARS = 50_000;
const MAX_INSTRUCTIONS_CHARS = 1_000;

interface PostProcessBody {
  task: string;
  text: string;
  language?: string;
  model_tier?: string;
  custom_instructions?: string | null;
}

export async function handlePostProcess(
  req: Request,
  env: Env,
  user: AuthenticatedUser,
): Promise<Response> {
  const idempotencyKey = req.headers.get("Idempotency-Key") ?? undefined;

  let body: PostProcessBody;
  try {
    body = (await req.json()) as PostProcessBody;
  } catch {
    return errorResponse("bad_request", "invalid JSON body");
  }

  if (!isValidTask(body.task)) {
    return errorResponse("bad_request", `unknown task: ${body.task}`);
  }
  if (typeof body.text !== "string" || !body.text.trim()) {
    return errorResponse("bad_request", "missing or empty 'text'");
  }
  if (body.text.length > MAX_INPUT_CHARS) {
    return errorResponse("payload_too_large", `text too long (max ${MAX_INPUT_CHARS} chars)`);
  }
  if (
    body.custom_instructions !== undefined &&
    body.custom_instructions !== null &&
    typeof body.custom_instructions !== "string"
  ) {
    return errorResponse("bad_request", "'custom_instructions' must be a string or null");
  }
  const customInstructions = (body.custom_instructions ?? "").trim();
  if (customInstructions.length > MAX_INSTRUCTIONS_CHARS) {
    return errorResponse(
      "bad_request",
      `custom_instructions too long (max ${MAX_INSTRUCTIONS_CHARS} chars)`,
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

  const template = getPromptTemplate(body.task);
  const userPrompt = template.buildUser(
    body.text,
    body.language,
    customInstructions || undefined,
  );

  let result;
  try {
    result = await chatCompletion(template.system, userPrompt, env, tier);
  } catch (err) {
    if (err instanceof OpenAIError) {
      // retryable (5xx + 429) → provider_unavailable; other 4xx → bad_request.
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
