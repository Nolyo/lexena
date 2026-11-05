import type { ReactNode, RefObject } from "react";
import { useTranslation } from "react-i18next";
import {
  Info,
  Keyboard,
  Mic,
  Palette,
  Plug,
  Settings,
  Sparkles,
  UserCircle2,
} from "lucide-react";

export interface NavItemDef {
  id: string;
  icon: ReactNode;
  iconBg: string;
  titleKey: string;
  subtitleKey: string;
}

/**
 * The top-level settings pages. Several former pages were folded into
 * these (see `resolveSettingsTarget` for the legacy → new mapping):
 *  - `section-dictee`   ← transcription + post-process + vocabulary
 *  - `section-raccourcis` also hosts the insertion-mode card (was in system)
 *  - `section-compte`   ← account + cloud (quota/billing)
 *  - `section-a-propos` ← about + updates
 */
export type SettingsSectionId =
  | "section-dictee"
  | "section-audio"
  | "section-apparence"
  | "section-raccourcis"
  | "section-compte"
  | "section-integrations"
  | "section-systeme"
  | "section-a-propos";

export const NAV_ITEM_DEFS: NavItemDef[] = [
  {
    id: "section-dictee",
    icon: <Sparkles className="w-3.5 h-3.5 text-vt-violet" />,
    iconBg: "bg-vt-violet/10",
    titleKey: "settings.nav.dictation",
    subtitleKey: "settings.nav.dictationSubtitle",
  },
  {
    id: "section-audio",
    icon: <Mic className="w-3.5 h-3.5 text-vt-cyan" />,
    iconBg: "bg-vt-cyan/10",
    titleKey: "settings.nav.audio",
    subtitleKey: "settings.nav.audioSubtitle",
  },
  {
    id: "section-apparence",
    icon: <Palette className="w-3.5 h-3.5 text-vt-pin" />,
    iconBg: "bg-vt-pin/10",
    titleKey: "settings.nav.appearance",
    subtitleKey: "settings.nav.appearanceSubtitle",
  },
  {
    id: "section-raccourcis",
    icon: <Keyboard className="w-3.5 h-3.5 text-vt-accent" />,
    iconBg: "bg-vt-accent/10",
    titleKey: "settings.nav.shortcuts",
    subtitleKey: "settings.nav.shortcutsSubtitle",
  },
  {
    id: "section-compte",
    icon: <UserCircle2 className="w-3.5 h-3.5 text-vt-accent" />,
    iconBg: "bg-vt-accent/10",
    titleKey: "auth.account.sectionTitle",
    subtitleKey: "auth.account.sectionSubtitle",
  },
  {
    id: "section-integrations",
    icon: <Plug className="w-3.5 h-3.5 text-vt-accent" />,
    iconBg: "bg-vt-accent/10",
    titleKey: "settings.nav.integrations",
    subtitleKey: "settings.nav.integrationsSubtitle",
  },
  {
    id: "section-systeme",
    icon: <Settings className="w-3.5 h-3.5 text-vt-warn" />,
    iconBg: "bg-vt-warn/10",
    titleKey: "settings.nav.system",
    subtitleKey: "settings.nav.systemSubtitle",
  },
  {
    id: "section-a-propos",
    icon: <Info className="w-3.5 h-3.5 text-vt-green" />,
    iconBg: "bg-vt-green/10",
    titleKey: "settings.nav.about",
    subtitleKey: "settings.nav.aboutSubtitle",
  },
];

// Keep backward compat alias
export const NAV_ITEMS = NAV_ITEM_DEFS;

/** Returns the nav items to display. (No auth-gated pages anymore — the cloud
 * card lives inside the account page, which handles its own signed-out state.) */
export function useNavItems(): NavItemDef[] {
  return NAV_ITEM_DEFS;
}

/**
 * Maps any settings id (including ids of pages that were merged away) to the
 * page that now hosts it, plus an optional anchor to scroll to within that
 * page. External callers (expiration popup, update modal, cross-section links)
 * keep emitting their original ids; this keeps those deep-links working.
 */
const LEGACY_SECTION_TARGETS: Record<
  string,
  { section: SettingsSectionId; anchor?: string }
> = {
  "section-transcription": { section: "section-dictee", anchor: "section-transcription" },
  "section-post-process": { section: "section-dictee", anchor: "section-post-process" },
  "section-vocabulaire": { section: "section-dictee", anchor: "section-vocabulaire" },
  "section-cloud": { section: "section-compte", anchor: "section-cloud" },
  "section-mcp": { section: "section-integrations", anchor: "section-mcp" },
  "section-mises-a-jour": { section: "section-a-propos", anchor: "section-mises-a-jour" },
};

export function resolveSettingsTarget(id: string): {
  section: SettingsSectionId;
  anchor?: string;
} {
  return LEGACY_SECTION_TARGETS[id] ?? { section: id as SettingsSectionId };
}

/** Smoothly scrolls to an anchored card after the section has rendered. */
export function scrollToSettingsAnchor(anchor: string): void {
  // Defer past the section swap so the target node exists in the DOM.
  requestAnimationFrame(() => {
    setTimeout(() => {
      document
        .getElementById(anchor)
        ?.scrollIntoView({ behavior: "smooth", block: "start" });
    }, 60);
  });
}

interface SettingsNavProps {
  activeId: string;
  scrollContainer: RefObject<HTMLDivElement | null>;
}

export function SettingsNav({ activeId, scrollContainer }: SettingsNavProps) {
  const { t } = useTranslation();
  const items = useNavItems();

  const scrollTo = (id: string) => {
    const el = document.getElementById(id);
    if (!el || !scrollContainer.current) return;
    const container = scrollContainer.current;
    const offset = el.offsetTop - container.offsetTop - 16;
    container.scrollTo({ top: offset, behavior: "smooth" });
  };

  return (
    <nav className="w-48 shrink-0 sticky top-0 self-start space-y-0.5 pt-0.5">
      {items.map((item) => (
        <button
          key={item.id}
          type="button"
          onClick={() => scrollTo(item.id)}
          className={`w-full flex items-center gap-2.5 px-2.5 py-2 rounded-lg text-left transition-all ${
            activeId === item.id
              ? "bg-muted text-foreground"
              : "text-muted-foreground hover:text-foreground hover:bg-muted/50"
          }`}
        >
          <div
            className={`w-7 h-7 rounded-md flex items-center justify-center shrink-0 ${item.iconBg}`}
          >
            {item.icon}
          </div>
          <div className="min-w-0">
            <p
              className={`text-xs font-semibold leading-none truncate ${
                activeId === item.id ? "text-foreground" : ""
              }`}
            >
              {t(item.titleKey)}
            </p>
            <p className="text-[10px] text-muted-foreground leading-tight mt-0.5 truncate">
              {t(item.subtitleKey)}
            </p>
          </div>
        </button>
      ))}
    </nav>
  );
}
