import { useEffect, useRef, type ReactNode } from "react";

export type SettingsSection = "sources" | "settings";

const focusableOverlayElement = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(", ");

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
  const dialogRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);

  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    const previouslyFocused =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;

    function onKeyDown(event: KeyboardEvent) {
      const dialog = dialogRef.current;
      if (!dialog) return;

      if (event.key === "Escape") {
        onCloseRef.current();
        return;
      }
      if (event.key !== "Tab") return;

      const nestedDialog = dialog.querySelector('[role="dialog"][aria-modal="true"]');
      if (nestedDialog) return;

      const focusable = Array.from(
        dialog.querySelectorAll<HTMLElement>(focusableOverlayElement),
      );
      if (focusable.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }

      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (event.shiftKey && (active === first || !dialog.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (active === last || !dialog.contains(active))) {
        event.preventDefault();
        first.focus();
      }
    }

    document.addEventListener("keydown", onKeyDown);
    dialogRef.current?.querySelector<HTMLElement>(focusableOverlayElement)?.focus();

    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (previouslyFocused?.isConnected) previouslyFocused.focus();
    };
  }, []);

  return (
    <div
      className="settings-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
      ref={dialogRef}
      tabIndex={-1}
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
