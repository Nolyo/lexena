import type { ReactNode } from "react";
import { ArrowRight, BadgeCheck, Check, Clock, Sparkles, UserPlus } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/hooks/useAuth";
import { useUsage } from "@/hooks/useUsage";
import { useDateFormatters } from "@/lib/date-format";

interface SubscriptionCardProps {
  onOpenAccountPage: () => void;
}

/** Quota resets on the 1st of each month — show the next occurrence. */
function firstOfNextMonth(): Date {
  const now = new Date();
  return new Date(now.getFullYear(), now.getMonth() + 1, 1);
}

/**
 * Account / subscription card for the home bento. Adapts to four states:
 * signed-out (create-account CTA), active plan (usage bar + renewal date),
 * active trial (minutes left + expiry), and signed-in-without-plan (upgrade
 * CTA). Branches on useAuth + useUsage.
 */
export function SubscriptionCard({ onOpenAccountPage }: SubscriptionCardProps) {
  const { t } = useTranslation();
  const { status, openAuthModal } = useAuth();
  const { trial, plan, ownerAccess, monthly_minutes_used, loading } = useUsage();
  const { formatLongDate, formatMonthDay } = useDateFormatters();

  // --- Signed-out: invite to create an account ----------------------------
  if (status === "signed-out") {
    return (
      <Card>
        <Header
          eyebrow={t("home.account.eyebrowSignedOut")}
          icon={<UserPlus className="w-4 h-4" />}
        />
        <h3 className="mt-3 vt-display text-[17px] font-semibold text-[var(--vt-fg)]">
          {t("home.account.titleSignedOut")}
        </h3>
        <p className="mt-1.5 text-[13px] text-[var(--vt-fg-3)] leading-relaxed">
          {t("home.account.descSignedOut")}
        </p>
        <Button className="mt-4" size="sm" onClick={openAuthModal}>
          {t("home.account.ctaSignedOut")}
          <ArrowRight className="w-4 h-4" />
        </Button>
      </Card>
    );
  }

  // Loading a signed-in user's usage: light skeleton.
  if (loading && !plan && !trial.is_active) {
    return (
      <Card>
        <Header
          eyebrow={t("home.account.eyebrowPlan")}
          icon={<BadgeCheck className="w-4 h-4" />}
        />
        <div className="mt-4 h-7 w-28 rounded-md bg-foreground/[0.06] animate-pulse" />
        <div className="mt-3 h-[7px] w-full rounded-full bg-foreground/[0.05] animate-pulse" />
      </Card>
    );
  }

  if (ownerAccess) {
    return (
      <Card>
        <Header eyebrow={t("cloud:owner.title")} icon={<BadgeCheck className="w-4 h-4" />} />
        <p className="mt-3 text-[13px] text-[var(--vt-fg-3)] leading-relaxed">{t("cloud:owner.description")}</p>
        <p className="mt-3 vt-mono text-[13px]">{t("cloud:owner.usage", {
          used: Math.round(monthly_minutes_used), quota: ownerAccess.monthly_minutes_limit,
        })}</p>
        <Foot onOpenAccountPage={onOpenAccountPage} />
      </Card>
    );
  }

  // --- Active subscription -------------------------------------------------
  if (plan) {
    const used = Math.round(monthly_minutes_used);
    const pct = plan.quota_minutes
      ? Math.min(100, Math.round((used / plan.quota_minutes) * 100))
      : 0;
    return (
      <Card>
        <Header
          eyebrow={t("home.account.eyebrowPlan")}
          badge={
            <StatusBadge color="var(--vt-green)" icon={<Check className="w-3 h-3" />}>
              {t("home.subscription.badgeActive")}
            </StatusBadge>
          }
        />
        <div className="mt-2.5 vt-display text-[21px] font-semibold text-[var(--vt-fg)]">
          {t(`home.account.plan_${plan.plan}`)}
        </div>
        <div className="mt-3.5 flex items-baseline justify-between gap-2">
          <span className="text-[13px] vt-mono text-[var(--vt-fg-2)]">
            <b className="text-[var(--vt-fg)] font-semibold">{used}</b> /{" "}
            {plan.quota_minutes} {t("home.account.minutesUnitShort")}
          </span>
          <span className="text-[11.5px] vt-mono text-[var(--vt-fg-4)]">{pct}%</span>
        </div>
        <div className="mt-2 h-[7px] rounded-full overflow-hidden bg-[var(--vt-surface)] border border-[var(--vt-border)]">
          <div
            className="h-full rounded-full"
            style={{
              width: `${pct}%`,
              background: "linear-gradient(90deg, var(--vt-accent-2), var(--vt-accent))",
            }}
          />
        </div>
        <Foot
          onOpenAccountPage={onOpenAccountPage}
          aside={t("home.subscription.renews", {
            date: formatMonthDay(firstOfNextMonth()),
          })}
        />
      </Card>
    );
  }

  // --- Active trial --------------------------------------------------------
  if (trial.is_active) {
    const minutesLeft = Math.max(0, Math.round(trial.minutes_remaining));
    return (
      <Card>
        <Header
          eyebrow={t("home.account.eyebrowTrial")}
          badge={
            <StatusBadge color="var(--vt-accent)" icon={<Sparkles className="w-3 h-3" />}>
              {t("home.subscription.badgeTrial")}
            </StatusBadge>
          }
        />
        <div className="mt-3 flex items-baseline gap-1.5">
          <span className="vt-display vt-mono text-[28px] leading-none font-semibold text-[var(--vt-fg)]">
            {minutesLeft}
          </span>
          <span className="text-[13px] text-[var(--vt-fg-3)]">
            {t("home.account.minutesUnit")}
          </span>
        </div>
        {trial.expires_at && (
          <p className="mt-2.5 flex items-center gap-1.5 text-[12.5px] text-[var(--vt-fg-3)]">
            <Clock className="w-3.5 h-3.5" />
            {t("home.account.trialExpires", {
              date: formatLongDate(new Date(trial.expires_at)),
            })}
          </p>
        )}
        <Foot onOpenAccountPage={onOpenAccountPage} />
      </Card>
    );
  }

  // --- Signed-in, no active plan or trial ---------------------------------
  return (
    <Card>
      <Header
        eyebrow={t("home.account.eyebrowInactive")}
        icon={<Sparkles className="w-4 h-4" />}
      />
      <h3 className="mt-3 vt-display text-[17px] font-semibold text-[var(--vt-fg)]">
        {t("home.account.titleInactive")}
      </h3>
      <p className="mt-1.5 text-[13px] text-[var(--vt-fg-3)] leading-relaxed">
        {t("home.account.descInactive")}
      </p>
      <Button className="mt-4" size="sm" onClick={onOpenAccountPage}>
        {t("home.account.ctaInactive")}
        <ArrowRight className="w-4 h-4" />
      </Button>
    </Card>
  );
}

function Card({ children }: { children: ReactNode }) {
  return <div className="vt-card-elevated p-5 flex flex-col">{children}</div>;
}

function Header({
  eyebrow,
  icon,
  badge,
}: {
  eyebrow: string;
  icon?: ReactNode;
  badge?: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="vt-eyebrow">{eyebrow}</span>
      {badge}
      {icon && (
        <span className="flex items-center justify-center w-8 h-8 rounded-lg shrink-0 bg-[var(--vt-accent-soft)] text-[var(--vt-accent)]">
          {icon}
        </span>
      )}
    </div>
  );
}

function StatusBadge({
  color,
  icon,
  children,
}: {
  color: string;
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <span
      className="inline-flex items-center gap-1 text-[11px] font-medium px-2 py-0.5 rounded-full"
      style={{
        color,
        background: `color-mix(in oklab, ${color} 14%, transparent)`,
        border: `1px solid color-mix(in oklab, ${color} 30%, transparent)`,
      }}
    >
      {icon}
      {children}
    </span>
  );
}

function Foot({
  onOpenAccountPage,
  aside,
}: {
  onOpenAccountPage: () => void;
  aside?: string;
}) {
  const { t } = useTranslation();
  return (
    <div className="mt-4 pt-3.5 border-t border-[var(--vt-border)] flex items-center justify-between gap-3">
      <button
        type="button"
        onClick={onOpenAccountPage}
        className="inline-flex items-center gap-1 text-[12.5px] font-medium text-[var(--vt-accent)] hover:underline cursor-pointer"
      >
        {t("home.account.manage")}
        <ArrowRight className="w-3.5 h-3.5" />
      </button>
      {aside && <span className="text-[11px] text-[var(--vt-fg-4)]">{aside}</span>}
    </div>
  );
}
