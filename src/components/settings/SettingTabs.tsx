import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { type SettingsSectionId } from "./common/SettingsNav";
import { DictationSection } from "./sections/DictationSection";
import { AudioSection } from "./sections/AudioSection";
import { SystemSection } from "./sections/SystemSection";
import { AppearanceSection } from "./sections/AppearanceSection";
import { ShortcutsSection } from "./sections/ShortcutsSection";
import { AboutSection } from "./sections/AboutSection";
import { AccountSection } from "./sections/AccountSection";
import { McpSection } from "./sections/McpSection";

interface SettingTabsProps {
  activeSection: SettingsSectionId;
  onSectionChange?: (id: SettingsSectionId) => void;
}

/**
 * Settings screen orchestrator.
 *
 * Renders only the section selected in the main sidebar. The sub-navigation
 * lives in DashboardSidebar (SettingsSidebarSection).
 */
export function SettingTabs({ activeSection, onSectionChange }: SettingTabsProps) {
  const { t } = useTranslation();
  const { isLoaded } = useSettings();

  if (!isLoaded) {
    return (
      <div className="flex items-center justify-center h-64">
        <p className="text-muted-foreground">{t('settings.loading')}</p>
      </div>
    );
  }

  return (
    <div className="vt-app mx-auto max-w-5xl px-3 py-6 sm:px-4 md:px-6">
      {activeSection === "section-dictee" && (
        <DictationSection onSectionChange={onSectionChange} />
      )}
      {activeSection === "section-audio" && <AudioSection />}
      {activeSection === "section-apparence" && <AppearanceSection />}
      {activeSection === "section-raccourcis" && <ShortcutsSection />}
      {activeSection === "section-compte" && <AccountSection />}
      {activeSection === "section-integrations" && <McpSection />}
      {activeSection === "section-systeme" && <SystemSection />}
      {activeSection === "section-a-propos" && <AboutSection />}
    </div>
  );
}
