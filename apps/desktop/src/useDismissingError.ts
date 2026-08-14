import { useEffect, useRef, useState } from "react";

const DEFAULT_DISMISS_MS = 6000;

export function useDismissingError(
  dismissAfterMs = DEFAULT_DISMISS_MS,
): [string | null, (value: string | null) => void] {
  const [error, setErrorState] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);

  function setError(value: string | null) {
    window.clearTimeout(timer.current);
    setErrorState(value);
    if (value !== null) {
      timer.current = window.setTimeout(
        () => setErrorState(null),
        dismissAfterMs,
      );
    }
  }

  useEffect(() => () => window.clearTimeout(timer.current), []);

  return [error, setError];
}
