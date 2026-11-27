import { useEffect } from "react";
import { create } from "zustand";
import type { Theme, WidgetSummary } from "./bindings";
import { useGlassdeckd } from "./glassdeckd";
import { freeSpot } from "./widgetArea";

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

/** The draft, started from the active theme once glassdeckd has sent it. */
export function useSeededDraft(): Theme | null {
  const active = useGlassdeckd((s) => s.theme);
  useEffect(() => {
    if (active) seedDraft(active);
  }, [active]);
  return useDraft((s) => s.draft);
}

/** Places `widget` in the draft's first free spot; its index in the draft's widgets, or null when there's no room. */
export function placeWidget(widget: WidgetSummary): number | null {
  const draft = useDraft.getState().draft;
  if (!draft) return null;
  const spot = freeSpot(widget.sizing.size, draft.widgets ?? []);
  if (!spot) return null;
  setDraft((d) => ({ ...d, widgets: [...(d.widgets ?? []), { package: widget.id, ...spot, options: {} }] }));
  return draft.widgets?.length ?? 0;
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
