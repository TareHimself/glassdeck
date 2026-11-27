import type { ScriptError } from "./bindings";

/** How an editor's preview run stands after the latest edit. */
export type RunStatus = { state: "running" } | { state: "held"; error: ScriptError } | { state: "failed"; error: ScriptError };

export const RUNNING: RunStatus = { state: "running" };

/** What the preview says over itself, if anything. */
export function runNote(status: RunStatus): string | null {
  switch (status.state) {
    case "running":
      return null;
    case "held":
      return "Showing your last working version";
    case "failed":
      return status.error.line === null ? "Stopped" : `Stopped at line ${status.error.line}`;
  }
}

/**
 * The error to mark in the editor: one that happened while running. A syntax error isn't, because the
 * language server already marks it as you type.
 */
export function markedError(status: RunStatus): ScriptError | null {
  return status.state !== "running" && !status.error.syntax && status.error.line !== null ? status.error : null;
}

/** An error as one log line: where it is, then what. */
export function errorLine(error: ScriptError): string {
  const at = [error.file, error.line].filter((part) => part !== null).join(":");
  return at ? `${at}: ${error.message}` : error.message;
}
