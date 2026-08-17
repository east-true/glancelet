import { useEffect, useRef, useState, type FormEvent } from "react";
import type { CapturePlanning } from "./api";
import { ErrorBanner } from "./ErrorBanner";

const planningOptions: { value: CapturePlanning; label: string }[] = [
  { value: "inbox", label: "Inbox" },
  { value: "today", label: "Today" },
  { value: "tomorrow", label: "Tomorrow" },
  { value: "backlog", label: "Backlog" },
];

export function QuickCapture({
  open,
  onClose,
  onCapture,
}: {
  open: boolean;
  onClose: () => void;
  onCapture: (
    requestId: string,
    title: string,
    planning: CapturePlanning,
  ) => Promise<void>;
}) {
  const [title, setTitle] = useState("");
  const [planning, setPlanning] = useState<CapturePlanning>("inbox");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const submittingRef = useRef(false);
  const requestIdRef = useRef<string | null>(null);

  useEffect(() => {
    if (!open) return;
    requestIdRef.current = crypto.randomUUID();
    window.setTimeout(() => inputRef.current?.focus(), 0);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [onClose, open]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (submittingRef.current) return;
    const normalized = title.trim();
    if (!normalized) {
      setError("Enter a title to capture.");
      inputRef.current?.focus();
      return;
    }
    submittingRef.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await onCapture(
        requestIdRef.current ?? crypto.randomUUID(),
        normalized,
        planning,
      );
      onClose();
    } catch (reason) {
      setError(String(reason));
    } finally {
      submittingRef.current = false;
      setSubmitting(false);
    }
  }

  if (!open) return null;

  return (
    <div className="quick-capture-layer">
      <form
        className="quick-capture-panel"
        role="dialog"
        aria-modal="true"
        aria-label="Quick Capture"
        onSubmit={(event) => void submit(event)}
      >
        <div className="quick-capture-heading">
          <div>
            <strong>Quick Capture</strong>
            <small>Keep the thought, then get back to work.</small>
          </div>
          <button
            type="button"
            className="btn-quiet"
            aria-label="Close Quick Capture"
            onClick={onClose}
          >
            ×
          </button>
        </div>
        <label className="quick-capture-field">
          <span>Title</span>
          <input
            ref={inputRef}
            aria-label="Capture title"
            autoComplete="off"
            maxLength={240}
            placeholder="What needs your attention?"
            value={title}
            onChange={(event) => setTitle(event.target.value)}
          />
        </label>
        <fieldset className="capture-planning">
          <legend>Add to</legend>
          {planningOptions.map((option) => (
            <label key={option.value}>
              <input
                type="radio"
                name="capture-planning"
                value={option.value}
                checked={planning === option.value}
                onChange={() => setPlanning(option.value)}
              />
              <span>{option.label}</span>
            </label>
          ))}
        </fieldset>
        <ErrorBanner message={error} />
        <div className="quick-capture-actions">
          <span>Enter to add · Esc to close</span>
          <button className="btn-primary" type="submit" disabled={submitting}>
            {submitting ? "Adding…" : "Add"}
          </button>
        </div>
      </form>
    </div>
  );
}
