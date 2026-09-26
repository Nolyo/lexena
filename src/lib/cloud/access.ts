/** Shared by the desktop UI and Worker; contains no client state or credentials. */
export interface CloudAccessGrant {
  monthly_minutes_limit: number;
  monthly_tokens_limit: number;
  expires_at: string | null;
}

export function activeCloudAccessGrant(value: unknown, now = Date.now()): CloudAccessGrant | null {
  if (!value || typeof value !== "object") return null;
  const row = value as Record<string, unknown>;
  if (row.revoked_at !== null) return null;
  if (row.expires_at !== null && (
    typeof row.expires_at !== "string" || !(Date.parse(row.expires_at) > now)
  )) return null;
  if (typeof row.monthly_minutes_limit !== "number" ||
      !Number.isInteger(row.monthly_minutes_limit) || row.monthly_minutes_limit <= 0 ||
      typeof row.monthly_tokens_limit !== "number" ||
      !Number.isInteger(row.monthly_tokens_limit) || row.monthly_tokens_limit <= 0) return null;
  return {
    monthly_minutes_limit: row.monthly_minutes_limit,
    monthly_tokens_limit: row.monthly_tokens_limit,
    expires_at: row.expires_at as string | null,
  };
}
