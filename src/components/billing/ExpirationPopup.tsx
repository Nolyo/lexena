import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { useCloud } from "@/hooks/useCloud";
import { BILLING_ENABLED } from "@/lib/billing/plans";

type Reason = "trial" | "subscription" | null;

/**
 * Heuristic detection of why cloud is no longer available.
 *
 * v3.2 scope: only "trial expired" is detected. Subscription expiry is
 * intentionally left to a future iteration — the `CloudContext` doesn't
 * keep an `wasActiveBefore` history yet, so we can't distinguish "never
 * subscribed" from "subscription just expired" without more state.
 *
 * Returns:
 *   - "trial"        if a trial is no longer active AND has expired AND no plan attached.
 *   - null           otherwise (still in trial, has active plan, or never had cloud).
 */
function detectReason(
  trial: { is_active: boolean; expires_at: string | null },
  plan: unknown,
): Reason {
  if (
    !plan &&
    !trial.is_active &&
    trial.expires_at &&
    new Date(trial.expires_at) < new Date()
  ) {
    return "trial";
  }
  return null;
}

export function ExpirationPopup() {
  const { t } = useTranslation("billing");
  const { trial, plan, isCloudEligible, usageLoading } = useCloud();
  const [dismissed, setDismissed] = useState(false);
  const [open, setOpen] = useState(false);

  const reason = isCloudEligible || usageLoading ? null : detectReason(trial, plan);

  useEffect(() => {
    if (reason && !dismissed) {
      setOpen(true);
    }
  }, [reason, dismissed]);

  if (!reason) return null;

  const handleSwitchLocal = () => {
    setOpen(false);
    setDismissed(true);
    // The popup just closes; the user's settings still carry their previous
    // provider choice. We intentionally don't mutate settings from here to
    // keep the popup non-destructive.
  };
  const handleLater = () => {
    setOpen(false);
    setDismissed(true);
  };
  const handleRenew = () => {
    setOpen(false);
    setDismissed(true);
    // Open settings → Cloud section so the user can reach the SubscribeButton.
    // Dashboard listens for this custom event and routes accordingly.
    window.dispatchEvent(
      new CustomEvent("lexena:open-settings", { detail: { section: "section-cloud" } }),
    );
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(v) => {
        if (!v) handleLater();
      }}
    >
      <DialogContent className="vt-app">
        <DialogTitle>
          {reason === "trial"
            ? t("expiration.trial_title")
            : t("expiration.subscription_title")}
        </DialogTitle>
        <DialogDescription>
          {reason === "trial"
            ? t("expiration.trial_body")
            : t("expiration.subscription_body")}
        </DialogDescription>
        {!BILLING_ENABLED && (
          <p className="mt-2 text-xs text-muted-foreground">
            {t("coming_soon.body")}
          </p>
        )}
        <DialogFooter className="gap-2">
          <Button variant="outline" onClick={handleSwitchLocal}>
            {t("expiration.switch_local_cta")}
          </Button>
          <Button variant="ghost" onClick={handleLater}>
            {t("expiration.later_cta")}
          </Button>
          <Button
            onClick={handleRenew}
            disabled={!BILLING_ENABLED}
            title={!BILLING_ENABLED ? t("coming_soon.title") : undefined}
          >
            {!BILLING_ENABLED ? t("coming_soon.cta") : t("expiration.renew_cta")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
