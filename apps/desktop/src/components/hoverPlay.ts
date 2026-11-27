import { useCallback, useEffect, useRef, useState } from "react";

/** How long a card is hovered or focused before its preview starts running, so passing over doesn't start one. */
export const HOVER_PLAY_DELAY_MS = 600;

/** Which card is playing: the one hovered or focused for `HOVER_PLAY_DELAY_MS`, until it's left. */
export function useHoverPlay() {
  const [live, setLive] = useState<string | null>(null);
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const start = useCallback((key: string) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setLive(key), HOVER_PLAY_DELAY_MS);
  }, []);
  const stop = useCallback((key: string) => {
    window.clearTimeout(timer.current);
    setLive((was) => (was === key ? null : was));
  }, []);
  return { live, start, stop };
}
