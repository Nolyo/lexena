import { useCallback, useEffect, useRef, useState } from "react";
import { mcpClient } from "@/lib/mcp/client";
import type { McpAuditEntry, McpStatus } from "@/lib/mcp/types";

type Outcome<T> = { ok: true; value: T } | { ok: false };

export function useMcpSettings() {
  // V1 native administration commands are registered only on Windows. The
  // Windows WebView reports its OS; no new plugin or cloud flag is required.
  const supported = /Windows/i.test(navigator.userAgent);
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [audit, setAudit] = useState<McpAuditEntry[] | null>(null);
  const [loading, setLoading] = useState(supported);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const mounted = useRef(false);

  const execute = useCallback(async <T,>(operation: () => Promise<T>, reload = true): Promise<Outcome<T>> => {
    if (inFlight.current) return { ok: false };
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      let outcome: { ok: true; value: T } | { ok: false; cause: unknown };
      try {
        outcome = { ok: true, value: await operation() };
      } catch (cause) {
        outcome = { ok: false, cause };
      }
      // Native administration can durably block/revoke a connection before a
      // vault operation fails. Always reload, retaining the action's own error.
      if (reload) {
        try {
          const next = await mcpClient.getStatus();
          if (mounted.current) setStatus(next);
        } catch (cause) {
          if (mounted.current) setStatus(null);
          if (outcome.ok) throw cause;
        }
      }
      if (!outcome.ok) throw outcome.cause;
      return outcome;
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause));
      return { ok: false };
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(false);
    }
  }, []);

  const refresh = useCallback(async () => {
    const outcome = await execute(mcpClient.getStatus, false);
    if (outcome.ok && mounted.current) setStatus(outcome.value);
    if (mounted.current) setLoading(false);
  }, [execute]);

  useEffect(() => {
    mounted.current = true;
    if (supported) void refresh();
    return () => { mounted.current = false; };
  }, [refresh, supported]);

  const refreshAudit = async () => {
    const outcome = await execute(mcpClient.getAudit, false);
    if (outcome.ok) setAudit(outcome.value);
  };

  return { supported, status, audit, loading, busy, error, execute, refresh, refreshAudit };
}
