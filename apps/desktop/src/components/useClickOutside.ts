import { useEffect, type RefObject } from "react";

/** While `open`, calls `close` when the pointer goes down anywhere outside `inside`. */
export function useClickOutside(open: boolean, inside: RefObject<HTMLElement | null>, close: () => void) {
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!inside.current?.contains(e.target as Node)) close();
    };
    window.addEventListener("pointerdown", onDown);
    return () => window.removeEventListener("pointerdown", onDown);
  }, [open, inside, close]);
}
