import { useTranslation } from "react-i18next";
import { useUsage } from "@/hooks/useUsage";
import { useAuth } from "@/hooks/useAuth";
import { Button } from "@/components/ui/button";
import { SubscribeButton } from "@/components/billing/SubscribeButton";

const LEMON_SQUEEZY_PORTAL_URL =
  (import.meta.env.VITE_LEMON_SQUEEZY_PORTAL_URL as string | undefined) ??
  "https://app.lemonsqueezy.com/my-orders";

function formatExpiry(iso: string | null): string {
  if (!iso) return "-";
  return new Date(iso).toLocaleDateString();
}

export function CloudSection() {
  const { t } = useTranslation("cloud");
  const { user } = useAuth();
  const { trial, monthly_minutes_breakdown, monthly_minutes_used, plan, ownerAccess, loading, refresh } = useUsage();

  if (!user) {
    return (
      <div id="section-cloud" className="vt-anim-fade-up space-y-5">
        <p className="text-sm text-muted-foreground">{t("settings.signin_required")}</p>
      </div>
    );
  }
  if (loading) {
    return (
      <div id="section-cloud" className="vt-anim-fade-up space-y-5">
        <p className="text-sm text-muted-foreground">{t("settings.loading")}</p>
      </div>
    );
  }

  const usedQuota = Math.floor(monthly_minutes_breakdown.quota);
  const trialMinutesLeft = Math.floor(trial.minutes_remaining);
  const expiryLabel = formatExpiry(trial.expires_at);

  return (
    <div id="section-cloud" className="vt-anim-fade-up space-y-5">
      <div className="vt-card-sectioned" style={{ overflow: "hidden" }}>
        <div className="vt-row flex flex-col gap-4 py-5">
          <h2 className="text-[15px] font-semibold tracking-tight">
            {t("settings.heading")}
          </h2>

          {ownerAccess && (
            <section className="space-y-2">
              <h3 className="text-[13px] font-semibold">{t("owner.title")}</h3>
              <p className="text-[12.5px] text-muted-foreground">{t("owner.description")}</p>
              <p className="text-[12.5px] vt-mono">{t("owner.usage", {
                used: Math.floor(monthly_minutes_used), quota: ownerAccess.monthly_minutes_limit,
              })}</p>
              <p className="text-[12.5px] text-muted-foreground">{t("owner.tokens", { count: ownerAccess.monthly_tokens_limit })}</p>
              {ownerAccess.expires_at && <p className="text-[12.5px] text-muted-foreground">
                {t("settings.bonus.expires_at", { date: formatExpiry(ownerAccess.expires_at) })}
              </p>}
            </section>
          )}

          {!ownerAccess && plan && (
            <section className="space-y-3">
              <h3 className="text-[13px] font-semibold">
                {t("settings.plan.heading", { plan: plan.plan })}
              </h3>
              <div className="flex items-center justify-between text-[12.5px]">
                <span className="text-muted-foreground">
                  {t("settings.plan.minutes_progress")}
                </span>
                <span className="vt-mono">
                  {t("settings.plan.minutes_progress_value", {
                    used: usedQuota,
                    quota: plan.quota_minutes,
                  })}
                </span>
              </div>
              <div
                className="h-1.5 w-full rounded-full bg-muted overflow-hidden"
                role="progressbar"
                aria-valuenow={usedQuota}
                aria-valuemin={0}
                aria-valuemax={plan.quota_minutes}
              >
                <div
                  className="h-full bg-primary"
                  style={{
                    width: `${Math.min((usedQuota / Math.max(plan.quota_minutes, 1)) * 100, 100)}%`,
                  }}
                />
              </div>

              {trial.is_active && (
                <div className="mt-2 border-t pt-3 space-y-1">
                  <p className="text-[12.5px] font-medium">{t("settings.bonus.heading")}</p>
                  <p className="text-[12.5px] text-muted-foreground">
                    {t("settings.bonus.minutes_remaining", {
                      count: trialMinutesLeft,
                      date: expiryLabel,
                    })}
                  </p>
                  <p className="text-[12px] text-muted-foreground italic">
                    {t("settings.bonus.consumed_first_note")}
                  </p>
                </div>
              )}
            </section>
          )}

          {!ownerAccess && !plan && trial.is_active && (
            <section className="space-y-2">
              <h3 className="text-[13px] font-semibold">{t("settings.bonus.heading")}</h3>
              <p className="text-[12.5px] text-muted-foreground">
                {t("settings.bonus.minutes_remaining_standalone", {
                  count: trialMinutesLeft,
                })}
              </p>
              <p className="text-[12.5px] text-muted-foreground">
                {t("settings.bonus.expires_at", { date: expiryLabel })}
              </p>
            </section>
          )}

          {!ownerAccess && !plan && !trial.is_active && (
            <p className="text-sm text-muted-foreground">{t("settings.nothing_active")}</p>
          )}

          {(!ownerAccess || plan) && <section className="space-y-3 border-t pt-5">
            <h3 className="text-[13px] font-semibold">
              {t("settings.section.plan_title")}
            </h3>
            {plan ? (
              <div className="rounded-md border p-4 text-sm">
                <p>
                  {t("settings.section.current_plan", {
                    tier: plan.plan,
                    quota: plan.quota_minutes,
                  })}
                </p>
                <Button asChild variant="outline" size="sm" className="mt-3">
                  <a
                    href={LEMON_SQUEEZY_PORTAL_URL}
                    target="_blank"
                    rel="noreferrer"
                  >
                    {t("settings.section.manage_cta")}
                  </a>
                </Button>
              </div>
            ) : (
              <SubscribeButton />
            )}
          </section>}

          <div>
            <button onClick={() => refresh()} className="vt-btn">
              {t("settings.refresh")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
