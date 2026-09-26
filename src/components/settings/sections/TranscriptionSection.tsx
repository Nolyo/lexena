import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import {
  OFFICIAL_LOCAL_MODEL,
  OFFICIAL_LOCAL_MODEL_SIZE_LABEL,
} from "@/lib/settings";
import { useSettings } from "@/hooks/useSettings";
import { useModelDownload } from "@/hooks/useModelDownload";
import { useCloud } from "@/hooks/useCloud";
import { useAuth } from "@/hooks/useAuth";
import {
  type SettingsSectionId,
  scrollToSettingsAnchor,
} from "../common/SettingsNav";
import {
  PickerCardGrid,
  Row,
  SectionHeader,
  Segmented,
  Toggle,
  VtIcon,
} from "../vt";

const ACCENT = "var(--vt-violet)";
const OFFICIAL_MODEL = OFFICIAL_LOCAL_MODEL;
const OFFICIAL_MODEL_SIZE = OFFICIAL_LOCAL_MODEL_SIZE_LABEL;

type Provider = "Local" | "LexenaCloud";

// Pretty display label for legacy model identifiers — only used in the
// migration banner below. We don't translate these because they're filenames /
// historical artifacts, not UI labels.
const LEGACY_MODEL_LABELS: Record<string, string> = {
  tiny: "Tiny (39 MB)",
  base: "Base (74 MB)",
  small: "Small (244 MB)",
  medium: "Medium (1.5 GB)",
  "large-v1": "Large v1 (2.9 GB)",
  "large-v2": "Large v2 (2.9 GB)",
  "large-v3": "Large v3 (2.9 GB)",
  "large-v3-turbo-q5_0": "Large v3 Turbo Q5 (547 MB)",
};

interface TranscriptionSectionProps {
  onSectionChange?: (id: SettingsSectionId) => void;
}

/**
 * Computes the cloud-eligibility chip displayed on the LexenaCloud provider
 * card so the user sees, at a glance, why picking cloud might or might not
 * work. Mirrors the gating logic in `useRecordingWorkflow` and the Rust
 * `cloud_gate` so the UI signal is consistent with what would happen on
 * record.
 */
function cloudStatusVariant({
  isSignedIn,
  isCloudEligible,
  trialActive,
  planName,
}: {
  isSignedIn: boolean;
  isCloudEligible: boolean;
  trialActive: boolean;
  planName: string | null;
}): { kind: "signin" | "trial" | "plan" | "none"; planName?: string } {
  if (!isSignedIn) return { kind: "signin" };
  if (isCloudEligible) {
    if (planName) return { kind: "plan", planName };
    if (trialActive) return { kind: "trial" };
  }
  return { kind: "none" };
}

export function TranscriptionSection({ onSectionChange }: TranscriptionSectionProps) {
  const { t } = useTranslation();
  const { settings, updateSetting } = useSettings();
  // Hardcoded to the only officially-supported model — quality of every other
  // model is too degraded to expose in the UI.
  const { isDownloading, progress, isDownloaded, isChecking, download, remove } =
    useModelDownload(settings.transcription_provider, OFFICIAL_MODEL);
  const { isCloudEligible, trial, plan, ownerAccess } = useCloud();
  const { user, openAuthModal } = useAuth();
  const [isMigrating, setIsMigrating] = useState(false);

  const isSignedIn = Boolean(user);
  const planName = plan?.plan ?? null;
  const status = cloudStatusVariant({
    isSignedIn,
    isCloudEligible,
    trialActive: trial.is_active,
    planName,
  });

  const cloudStatusBadge =
    isSignedIn && ownerAccess ? (
      <ProviderBadge color="var(--vt-ok)">{t("cloud:owner.title")}</ProviderBadge>
    ) : status.kind === "signin" ? (
      <ProviderBadge color="var(--vt-danger)">
        {t("settings.transcription.cloudStatus.signinRequired")}
      </ProviderBadge>
    ) : status.kind === "plan" ? (
      <ProviderBadge color="var(--vt-ok)">
        {t("settings.transcription.cloudStatus.planActive", {
          plan: status.planName,
        })}
      </ProviderBadge>
    ) : status.kind === "trial" ? (
      <ProviderBadge color="var(--vt-ok)">
        {t("settings.transcription.cloudStatus.trialActive")}
      </ProviderBadge>
    ) : (
      <ProviderBadge color="var(--vt-warn)">
        {t("settings.transcription.cloudStatus.noActivePlan")}
      </ProviderBadge>
    );

  const providerOptions = [
    {
      id: "Local" as Provider,
      title: t("settings.transcription.providerLocal"),
      sub: t("settings.transcription.providerLocalSub"),
      dot: "var(--vt-cyan)",
    },
    {
      id: "LexenaCloud" as Provider,
      title: t("settings.transcription.providerLexenaCloud"),
      sub: t("settings.transcription.providerLexenaCloudSub"),
      dot: "var(--vt-accent)",
      badge: (
        <div className="flex items-center gap-1.5">
          <ProviderBadge color="var(--vt-accent)">
            {t("settings.transcription.providerBetaBadge")}
          </ProviderBadge>
          {cloudStatusBadge}
        </div>
      ),
    },
  ];

  const cloudSelected = settings.transcription_provider === "LexenaCloud";
  const showCloudBanner = cloudSelected && !isCloudEligible;

  return (
    <div className="vt-anim-fade-up space-y-5">
      <div className="vt-card-sectioned" style={{ overflow: "hidden" }}>
        <SectionHeader
          color={ACCENT}
          icon={<VtIcon.sparkle />}
          title={t("settings.transcription.title")}
          description={t("settings.transcription.subtitle")}
        />

        <Row
          label={t("settings.transcription.provider")}
          hint={t("settings.transcription.providerHint")}
        >
          <div className="flex flex-col gap-2">
            <PickerCardGrid
              value={settings.transcription_provider}
              onChange={(v) => updateSetting("transcription_provider", v)}
              options={providerOptions}
              columns={2}
            />
            {showCloudBanner && (
              <CloudEligibilityBanner
                isSignedIn={isSignedIn}
                onSignIn={() => openAuthModal()}
                onManagePlan={() => {
                  onSectionChange?.("section-compte");
                  scrollToSettingsAnchor("section-cloud");
                }}
                t={t}
              />
            )}
          </div>
        </Row>

        <Row
          label={t("settings.transcription.streaming.title")}
          hint={t("settings.transcription.streaming.description")}
        >
          {cloudSelected ? (
            <Toggle
              on={settings.streaming_mode}
              onClick={() =>
                updateSetting("streaming_mode", !settings.streaming_mode)
              }
              label={
                settings.streaming_mode
                  ? t("common.enabled", { defaultValue: "Activé" })
                  : t("common.disabled", { defaultValue: "Désactivé" })
              }
            />
          ) : (
            <div className="flex items-center gap-2.5">
              <Toggle
                on={false}
                onClick={() => {}}
                disabled
                label={t("common.disabled", { defaultValue: "Désactivé" })}
              />
              <ProviderBadge color="var(--vt-accent)">
                {t("settings.transcription.streaming.cloudOnly")}
              </ProviderBadge>
            </div>
          )}
        </Row>

        <Row
          label={t("settings.transcription.language")}
          hint={t("settings.transcription.languageHint", {
            defaultValue:
              "Langue parlée principale. Améliore la précision de la transcription.",
          })}
        >
          <select
            className="vt-select"
            value={settings.language}
            onChange={(e) => updateSetting("language", e.target.value)}
            style={{ maxWidth: 240 }}
          >
            <option value="fr-FR">{t("settings.transcription.languageFr")}</option>
            <option value="en-US">{t("settings.transcription.languageEn")}</option>
            <option value="es-ES">{t("settings.transcription.languageEs")}</option>
            <option value="de-DE">{t("settings.transcription.languageDe")}</option>
          </select>
        </Row>

        {settings.transcription_provider === "Local" && (
          <>
            {settings.local_model_size !== OFFICIAL_MODEL && (
              <LegacyModelMigrationBanner
                currentModel={settings.local_model_size}
                isMigrating={isMigrating}
                onMigrate={async () => {
                  const oldModel = settings.local_model_size;
                  setIsMigrating(true);
                  try {
                    await invoke("download_local_model", {
                      model: OFFICIAL_MODEL,
                    });
                    await updateSetting("local_model_size", OFFICIAL_MODEL);
                    if (oldModel && oldModel !== OFFICIAL_MODEL) {
                      try {
                        await invoke("delete_local_model", { model: oldModel });
                      } catch (e) {
                        console.warn("Failed to delete legacy model:", e);
                      }
                    }
                    toast.success(
                      t("settings.transcription.legacyMigration.success"),
                    );
                  } catch (error) {
                    console.error("Migration failed:", error);
                    toast.error(
                      t("settings.transcription.legacyMigration.failure", {
                        error: String(error),
                      }),
                    );
                  } finally {
                    setIsMigrating(false);
                  }
                }}
                t={t}
              />
            )}

            <Row
              label={t("settings.transcription.whisperModel")}
              hint={t("settings.transcription.officialModelHint")}
            >
              <div className="flex items-center justify-between gap-2">
                <div className="flex items-center gap-2 min-w-0">
                  <span className="text-[13px] font-medium">
                    {t("settings.transcription.officialModelName")}
                  </span>
                  <span
                    className="text-[12px]"
                    style={{ color: "var(--vt-fg-3)" }}
                  >
                    · {OFFICIAL_MODEL_SIZE}
                  </span>
                </div>

                <div className="flex items-center gap-2 shrink-0">
                  {isDownloaded ? (
                    <>
                      <span
                        className="inline-flex items-center gap-1.5 h-9 px-3 rounded-md text-[12px] font-medium"
                        style={{
                          color: "var(--vt-ok)",
                          background: "var(--vt-ok-soft)",
                          border: "1px solid oklch(from var(--vt-ok) l c h / 0.3)",
                        }}
                      >
                        <VtIcon.check /> {t("settings.transcription.installed")}
                      </span>
                      <button
                        type="button"
                        onClick={remove}
                        className="vt-btn"
                        data-tip={t("settings.transcription.deleteModel")}
                        style={{
                          color: "var(--vt-danger)",
                          borderColor: "oklch(from var(--vt-danger) l c h / 0.3)",
                        }}
                      >
                        <VtIcon.trash />
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      onClick={download}
                      disabled={isDownloading || isChecking}
                      className="vt-btn-primary"
                    >
                      {isDownloading ? (
                        <>
                          <VtIcon.spinner />
                          {Math.round(progress)}%
                        </>
                      ) : (
                        <>
                          <VtIcon.refresh />
                          {t("settings.transcription.download")}
                        </>
                      )}
                    </button>
                  )}
                </div>
              </div>
              {isDownloading && (
                <div
                  className="mt-2 h-1.5 rounded-full overflow-hidden"
                  style={{ background: "var(--vt-surface)" }}
                >
                  <div
                    className="h-full"
                    style={{
                      width: `${progress}%`,
                      background: "var(--vt-accent)",
                      transition: "width .2s",
                    }}
                  />
                </div>
              )}
            </Row>

            {isDownloaded && (
              <Row
                label={t("settings.transcription.keepModelInMemory")}
                hint={t("settings.transcription.keepModelInMemoryDesc")}
              >
                <Segmented
                  value={
                    settings.keep_model_in_memory === null
                      ? "auto"
                      : settings.keep_model_in_memory
                        ? "true"
                        : "false"
                  }
                  onChange={(v) => {
                    const mapped = v === "auto" ? null : v === "true";
                    updateSetting("keep_model_in_memory", mapped);
                  }}
                  options={[
                    { id: "auto", label: t("settings.transcription.keepModelInMemoryAuto") },
                    { id: "true", label: t("common.yes") },
                    { id: "false", label: t("common.no") },
                  ]}
                />
              </Row>
            )}
          </>
        )}

        <Row
          label={t("settings.transcription.smartFormatting")}
          hint={t("settings.transcription.smartFormattingHint")}
        >
          <Toggle
            on={settings.smart_formatting}
            onClick={() =>
              updateSetting("smart_formatting", !settings.smart_formatting)
            }
            label={
              settings.smart_formatting
                ? t("common.enabled", { defaultValue: "Activé" })
                : t("common.disabled", { defaultValue: "Désactivé" })
            }
          />
        </Row>
      </div>
    </div>
  );
}

interface ProviderBadgeProps {
  color: string;
  children: ReactNode;
}

function ProviderBadge({ color, children }: ProviderBadgeProps) {
  return (
    <span
      className="inline-flex items-center px-1.5 py-0.5 rounded text-[10px] font-semibold uppercase tracking-wide"
      style={{
        color,
        background: `oklch(from ${color} l c h / 0.16)`,
        border: `1px solid oklch(from ${color} l c h / 0.32)`,
      }}
    >
      {children}
    </span>
  );
}

interface LegacyModelMigrationBannerProps {
  currentModel: string;
  isMigrating: boolean;
  onMigrate: () => void;
  t: (key: string, opts?: Record<string, unknown>) => string;
}

function LegacyModelMigrationBanner({
  currentModel,
  isMigrating,
  onMigrate,
  t,
}: LegacyModelMigrationBannerProps) {
  const prettyName = LEGACY_MODEL_LABELS[currentModel] ?? currentModel;
  return (
    <div
      className="rounded-lg px-3 py-2.5 flex items-start gap-3 mx-4 my-3"
      style={{
        background: "oklch(from var(--vt-warn) l c h / 0.10)",
        border: "1px solid oklch(from var(--vt-warn) l c h / 0.32)",
      }}
    >
      <span className="mt-0.5 shrink-0" style={{ color: "var(--vt-warn)" }}>
        <VtIcon.alert />
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-[12.5px] font-semibold">
          {t("settings.transcription.legacyMigration.title", {
            model: prettyName,
          })}
        </div>
        <div
          className="text-[12px] mt-0.5 leading-relaxed"
          style={{ color: "var(--vt-fg-3)" }}
        >
          {t("settings.transcription.legacyMigration.body")}
        </div>
      </div>
      <button
        type="button"
        onClick={onMigrate}
        disabled={isMigrating}
        className="vt-btn-primary shrink-0"
        style={{ height: 30 }}
      >
        {isMigrating ? (
          <>
            <VtIcon.spinner />
            {t("settings.transcription.legacyMigration.migrating")}
          </>
        ) : (
          t("settings.transcription.legacyMigration.cta")
        )}
      </button>
    </div>
  );
}

interface CloudEligibilityBannerProps {
  isSignedIn: boolean;
  onSignIn: () => void;
  onManagePlan: () => void;
  t: (key: string, opts?: Record<string, unknown>) => string;
}

function CloudEligibilityBanner({
  isSignedIn,
  onSignIn,
  onManagePlan,
  t,
}: CloudEligibilityBannerProps) {
  const titleKey = isSignedIn
    ? "settings.transcription.cloudBanner.noPlanTitle"
    : "settings.transcription.cloudBanner.signinRequiredTitle";
  const bodyKey = isSignedIn
    ? "settings.transcription.cloudBanner.noPlanBody"
    : "settings.transcription.cloudBanner.signinRequiredBody";

  return (
    <div
      className="rounded-lg px-3 py-2.5 flex items-start gap-3"
      style={{
        background: "oklch(from var(--vt-warn) l c h / 0.10)",
        border: "1px solid oklch(from var(--vt-warn) l c h / 0.32)",
      }}
    >
      <span
        className="mt-0.5 shrink-0"
        style={{ color: "var(--vt-warn)" }}
      >
        <VtIcon.alert />
      </span>
      <div className="flex-1 min-w-0">
        <div className="text-[12.5px] font-semibold">{t(titleKey)}</div>
        <div
          className="text-[12px] mt-0.5 leading-relaxed"
          style={{ color: "var(--vt-fg-3)" }}
        >
          {t(bodyKey)}
        </div>
      </div>
      <button
        type="button"
        onClick={isSignedIn ? onManagePlan : onSignIn}
        className="vt-btn-primary shrink-0"
        style={{ height: 30 }}
      >
        {isSignedIn
          ? t("settings.transcription.cloudBanner.managePlanCta")
          : t("settings.transcription.cloudBanner.signinCta")}
      </button>
    </div>
  );
}
