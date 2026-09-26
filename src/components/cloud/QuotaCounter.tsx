import { useTranslation } from "react-i18next";
import { useUsage } from "@/hooks/useUsage";
import { useAuth } from "@/hooks/useAuth";
import { useCloud } from "@/hooks/useCloud";

function daysUntil(iso: string | null): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime() - Date.now();
  return ms > 0 ? Math.ceil(ms / (24 * 3600 * 1000)) : 0;
}

export function QuotaCounter() {
  const { t } = useTranslation("cloud");
  const { user } = useAuth();
  const { hasCloudSelected } = useCloud();
  const { trial, monthly_minutes_breakdown, monthly_minutes_used, plan, ownerAccess, loading } = useUsage();

  // Only surface the quota pill when the user actually opted into Lexena
  // Cloud. Showing it for users still on Local / their own keys would suggest
  // the cloud is consuming their quota when it isn't.
  if (!user || !hasCloudSelected || loading) return null;

  if (ownerAccess) {
    return (
      <div className="text-xs vt-mono text-muted-foreground px-2 py-1 rounded-md border border-border bg-card/40"
        title={t("owner.description")}>
        {t("owner.title")} · {t("plan.minutes_remaining", {
          count: Math.floor(Math.max(0, ownerAccess.monthly_minutes_limit - monthly_minutes_used)),
        })}
      </div>
    );
  }

  if (trial.is_active) {
    const days = daysUntil(trial.expires_at);
    const showMinutes = days === null || trial.minutes_remaining < days * 2;
    return (
      <div
        className="text-xs vt-mono text-muted-foreground px-2 py-1 rounded-md border border-border bg-card/40"
        title={t("trial.tooltip")}
      >
        {showMinutes
          ? t("trial.minutes_remaining", { count: Math.floor(trial.minutes_remaining) })
          : t("trial.days_remaining", { count: days ?? 0 })}
      </div>
    );
  }

  if (plan) {
    const remaining = Math.max(
      plan.quota_minutes - Math.floor(monthly_minutes_breakdown.quota),
      0,
    );
    return (
      <div
        className="text-xs vt-mono text-muted-foreground px-2 py-1 rounded-md border border-border bg-card/40"
        title={t("plan.tooltip", { plan: plan.plan })}
      >
        {t("plan.minutes_remaining", { count: Math.floor(remaining) })}
      </div>
    );
  }

  return null;
}
