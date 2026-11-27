import { useEffect, useRef, useState } from "react";
import { commands, type MessageProblem } from "./bindings";

export const MESSAGES_SCRIPT = "messages.luau";
/** How long after the last keystroke in `messages.luau` the types are written again */
const SETTLE_MS = 400;

/** Only whitespace and comments, as packing treats a script that does nothing. */
export function isBlankScript(text: string): boolean {
  return text.replace(/--\[(=*)\[[\s\S]*?\]\1\]/g, "").replace(/--[^\n]*/g, "").trim() === "";
}

/**
 * Keeps the package's message types in step with the editor's `messages.luau` as it's edited, before a
 * save, so the language servers type the scripts' messages. `version` goes up each time they're
 * rewritten; `problems` are `messages.luau`'s, each at its place.
 */
export function useMessageTypes(id: string, files: Record<string, string>, loaded: boolean) {
  const [types, setTypes] = useState<{ version: number; problems: MessageProblem[] }>({ version: 0, problems: [] });
  const messages = files[MESSAGES_SCRIPT];
  const hostBlank = isBlankScript(files["host.luau"] ?? "");
  const written = useRef<{ messages: string | undefined; hostBlank: boolean } | null>(null);

  useEffect(() => {
    if (!loaded || messages === undefined) return;
    if (written.current === null) {
      written.current = { messages, hostBlank };
      return;
    }
    if (written.current.messages === messages && written.current.hostBlank === hostBlank) return;
    const timer = window.setTimeout(() => {
      written.current = { messages, hostBlank };
      void commands.refreshMessageTypes(id, messages, hostBlank).then((result) => {
        if (result.status === "ok") setTypes((t) => ({ version: t.version + 1, problems: result.data }));
        else console.error("writing the message types:", result.error);
      });
    }, SETTLE_MS);
    return () => window.clearTimeout(timer);
  }, [id, loaded, messages, hostBlank]);

  return types;
}
