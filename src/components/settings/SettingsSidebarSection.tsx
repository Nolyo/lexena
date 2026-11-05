import { useTranslation } from "react-i18next";
import { useNavItems, type SettingsSectionId } from "./common/SettingsNav";

interface SettingsSidebarSectionProps {
  activeSection: SettingsSectionId;
  onSectionChange: (id: SettingsSectionId) => void;
  collapsed?: boolean;
}

export function SettingsSidebarSection({
  activeSection,
  onSectionChange,
  collapsed = false,
}: SettingsSidebarSectionProps) {
  const { t } = useTranslation();
  const navItems = useNavItems();

  if (collapsed) {
    return (
      <div className="flex flex-col border-t border-border overflow-y-auto flex-1 min-h-0 p-1 gap-1 items-center">
        {navItems.map((item) => {
          const isActive = activeSection === item.id;
          return (
            <button
              key={item.id}
              type="button"
              aria-current={isActive ? "page" : undefined}
              onClick={() => onSectionChange(item.id as SettingsSectionId)}
              title={t(item.titleKey)}
              className="flex items-center justify-center p-1 rounded-md transition-colors cursor-pointer hover:bg-accent"
              style={isActive ? { background: "var(--vt-accent-soft)" } : undefined}
            >
              <div
                className={`w-6 h-6 rounded-md flex items-center justify-center shrink-0 ${item.iconBg}`}
              >
                {item.icon}
              </div>
            </button>
          );
        })}
      </div>
    );
  }

  return (
    <div className="flex flex-col border-t border-border overflow-y-auto flex-1 min-h-0 p-2 gap-0.5">
      {navItems.map((item) => {
        const isActive = activeSection === item.id;
        return (
          <button
            key={item.id}
            type="button"
            aria-current={isActive ? "page" : undefined}
            onClick={() => onSectionChange(item.id as SettingsSectionId)}
            className={`w-full flex items-center gap-2.5 px-2 py-2 rounded-md text-left transition-colors cursor-pointer ${
              isActive
                ? "text-foreground"
                : "text-muted-foreground hover:text-foreground hover:bg-accent"
            }`}
            style={
              isActive
                ? {
                    background: "var(--vt-accent-soft)",
                    boxShadow: "inset 0 0 0 1px oklch(from var(--vt-accent) l c h / 0.25)",
                  }
                : undefined
            }
          >
            <div
              className={`w-7 h-7 rounded-md flex items-center justify-center shrink-0 ${item.iconBg}`}
            >
              {item.icon}
            </div>
            <div className="min-w-0 flex-1">
              <p
                className="text-xs font-semibold leading-none truncate"
                style={isActive ? { color: "var(--vt-accent-2)" } : undefined}
              >
                {t(item.titleKey)}
              </p>
              <p
                className="text-[10px] leading-tight mt-0.5 truncate"
                style={{ color: isActive ? "var(--vt-fg-2)" : "var(--vt-fg-4)" }}
              >
                {t(item.subtitleKey)}
              </p>
            </div>
          </button>
        );
      })}
    </div>
  );
}
