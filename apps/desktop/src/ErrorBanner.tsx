import { useEffect, useRef, useState } from "react";

const FADE_MS = 180;

export function ErrorBanner({ message }: { message: string | null }) {
  const [rendered, setRendered] = useState(message);
  const [visible, setVisible] = useState(false);
  const clearTimer = useRef<number | undefined>(undefined);

  if (message !== null && message !== rendered) {
    setRendered(message);
  }

  useEffect(() => {
    window.clearTimeout(clearTimer.current);
    if (message !== null) {
      const frame = requestAnimationFrame(() => setVisible(true));
      return () => cancelAnimationFrame(frame);
    }
    const frame = requestAnimationFrame(() => setVisible(false));
    clearTimer.current = window.setTimeout(() => setRendered(null), FADE_MS);
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(clearTimer.current);
    };
  }, [message]);

  if (!rendered) return null;

  return (
    <p className={`error-banner${visible ? " is-visible" : ""}`}>{rendered}</p>
  );
}
