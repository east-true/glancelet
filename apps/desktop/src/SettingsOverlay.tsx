import { useEffect, type ReactNode } from "react";

export type SettingsSection = "sources" | "settings";

export function SettingsOverlay({
  section,
  onSection,
  onClose,
  children,
}: {
  section: SettingsSection;
  onSection: (section: SettingsSection) => void;
  onClose: () => void;
  children: ReactNode;
}) {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="settings-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
    >
      <div className="settings-overlay-header">
        <nav className="tabs" aria-label="Settings sections">
          <button
            className={section === "sources" ? "active" : ""}
            onClick={() => onSection("sources")}
          >
            Sources
          </button>
          <button
            className={section === "settings" ? "active" : ""}
            onClick={() => onSection("settings")}
          >
            Settings
          </button>
        </nav>
        <button
          type="button"
          className="btn-quiet modal-close"
          aria-label="Close settings"
          onClick={onClose}
        >
          ×
        </button>
      </div>
      <div className="settings-overlay-body">{children}</div>
    </div>
  );
}
