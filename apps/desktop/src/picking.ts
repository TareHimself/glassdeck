import { useRef, useState } from "react";
import type { LibraryError } from "./bindings";

/** A picked file and what loading it gave. */
export type Chosen<T> = { path: string; value: T };

/**
 * Picking a file and loading it. While the dialog is open or the file loads, the last chosen one is kept,
 * so cancelling or a failed load goes back to it.
 */
export type Picking<T> =
  | { state: "empty" }
  | { state: "choosing"; last: Chosen<T> | null }
  | { state: "loading"; path: string; last: Chosen<T> | null }
  | { state: "chosen"; chosen: Chosen<T> };

type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };

export function lastChosen<T>(picking: Picking<T>): Chosen<T> | null {
  switch (picking.state) {
    case "empty":
      return null;
    case "choosing":
    case "loading":
      return picking.last;
    case "chosen":
      return picking.chosen;
  }
}

function settled<T>(last: Chosen<T> | null): Picking<T> {
  return last ? { state: "chosen", chosen: last } : { state: "empty" };
}

/**
 * `pick` opens the file dialog (null when cancelled) and `load` reads what was picked. A second
 * `choose` while one is under way is ignored, so the dialog never opens twice.
 */
export function usePicker<T>(pick: () => Promise<string | null>, load: (path: string) => Promise<Answer<T>>) {
  const [picking, setPicking] = useState<Picking<T>>({ state: "empty" });
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const busy = useRef(false);

  const choose = async () => {
    if (busy.current) return;
    busy.current = true;
    const last = lastChosen(picking);
    try {
      setPicking({ state: "choosing", last });
      const path = await pick();
      if (!path) return setPicking(settled(last));
      setPicking({ state: "loading", path, last });
      const loaded = await load(path);
      if (loaded.status === "error") {
        setProblem(loaded.error);
        return setPicking(settled(last));
      }
      setProblem(null);
      setPicking({ state: "chosen", chosen: { path, value: loaded.data } });
    } finally {
      busy.current = false;
    }
  };

  return { picking, choose, problem, busy: picking.state === "choosing" || picking.state === "loading" };
}

/** What a Choose button says while picking is under way. */
export function chooseLabel(picking: Picking<unknown>, idle: string): string {
  switch (picking.state) {
    case "choosing":
      return "Choosing…";
    case "loading":
      return "Loading…";
    default:
      return idle;
  }
}
