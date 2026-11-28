import { useState } from "react";
import { create } from "zustand";
import { commands, type Theme } from "./bindings";
import { useDraft } from "./draft";
import { describeError, keyboardProblem, useGlassdeckd } from "./glassdeckd";
import { describeLibraryError } from "./library";

/**
 * Saves of each package since it last went to the keyboard. Apply detection isn't built, so the app counts
 * them itself while it runs; at start, what the keyboard shows counts as up to date.
 */
type AppliedStore = { saves: Record<string, number>; applied: Record<string, number> };

export const useApplied = create<AppliedStore>(() => ({ saves: {}, applied: {} }));

export function noteSaved(id: string) {
  useApplied.setState(({ saves }) => ({ saves: { ...saves, [id]: (saves[id] ?? 0) + 1 } }));
}

function noteApplied(id: string) {
  useApplied.setState(({ saves, applied }) => ({ applied: { ...applied, [id]: saves[id] ?? 0 } }));
}

export type ApplyState = "set up" | "on keyboard" | "ready";

/** Whether a background editor's Apply to keyboard can send it. */
export function applyState(at: { needsSetup: boolean; onKeyboard: boolean; unsaved: boolean; saves: number; applied: number }): ApplyState {
  if (at.needsSetup) return "set up";
  return at.onKeyboard && !at.unsaved && at.saves === at.applied ? "on keyboard" : "ready";
}

/** Sends a theme to the keyboard, as the Overview's Apply does; what went wrong, worded for the user. */
export function useApplyTheme() {
  const connection = useGlassdeckd((s) => s.connection);
  const [applying, setApplying] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const apply = async (theme: Theme): Promise<boolean> => {
    setApplying(true);
    const result = await commands.applyTheme(theme);
    setApplying(false);
    if (result.status === "ok") {
      noteApplied(theme.background.package);
      setProblem(null);
      return true;
    }
    const error = result.error;
    const keyboard = connection.state === "connected" ? keyboardProblem(connection.status.link.connection) : null;
    setProblem(error.kind === "glassdeckd" ? (keyboard ?? describeError(error.error)) : describeLibraryError(error.error));
    return false;
  };

  return { apply, applying, problem };
}

/** The draft with `id` as its background, keeping that background's chosen options if it already was. */
export function withBackground(theme: Theme, id: string): Theme {
  if (theme.background.package === id) return theme;
  return { ...theme, background: { package: id, options: {} } };
}

/** Makes `id` the draft's background and applies the draft. */
export async function applyBackground(id: string, apply: (theme: Theme) => Promise<boolean>): Promise<boolean> {
  const draft = useDraft.getState().draft ?? useGlassdeckd.getState().theme;
  if (!draft) return false;
  const theme = withBackground(draft, id);
  useDraft.setState({ draft: theme });
  return apply(theme);
}
