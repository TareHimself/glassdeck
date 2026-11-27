import { create } from "zustand";
import type { Theme } from "./bindings";

/**
 * The setup the Overview shows and edits. Edits change only this; Apply to keyboard sends it. It starts as
 * a copy of the active theme and lasts while the app runs.
 */
type DraftStore = { draft: Theme | null };

export const useDraft = create<DraftStore>(() => ({ draft: null }));

export function setDraft(change: (draft: Theme) => Theme) {
  useDraft.setState(({ draft }) => ({ draft: draft && change(draft) }));
}

/** Starts the draft from the active theme the first time there is one. */
export function seedDraft(active: Theme) {
  useDraft.setState(({ draft }) => ({ draft: draft ?? active }));
}

/** The same JSON whatever order its object keys came in. */
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>).sort(([a], [b]) => a.localeCompare(b));
    return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
  }
  return JSON.stringify(value ?? null);
}

export function sameTheme(a: Theme, b: Theme): boolean {
  return canonical(a) === canonical(b);
}
