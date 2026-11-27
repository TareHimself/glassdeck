import * as monaco from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import type { LspServer } from "../bindings";
import { LanguageClient, serverFor } from "./client";
import { registerLanguageProviders, serve } from "./monaco";

/** What a source panel needs to run language servers: whose files, and when `.glassdeck/` is written. */
export type LanguageSupport = {
  packageId: string;
  /** Changes each time a check finishes, which rewrites the definitions; null until the first one does */
  definitions: object | null;
};

type Running = { clients: Map<LspServer, Promise<LanguageClient>>; served: Map<string, () => void> };

/**
 * Runs the server for each tab the first time it's shown, and restarts them all whenever the definitions
 * are rewritten. Returns why language support is off, when it is.
 */
export function useLanguageServers(support: LanguageSupport | undefined, tab: string, editorMounted: boolean): string | null {
  const running = useRef<Running | null>(null);
  const [unavailable, setUnavailable] = useState<string | null>(null);
  const packageId = support?.packageId;
  const definitions = support?.definitions ?? null;

  useEffect(() => {
    if (!packageId || !definitions) return;
    registerLanguageProviders();
    const own: Running = { clients: new Map(), served: new Map() };
    running.current = own;
    setUnavailable(null);
    return () => {
      if (running.current === own) running.current = null;
      for (const stopServing of own.served.values()) stopServing();
      for (const client of own.clients.values()) void client.then((c) => c.stop()).catch(() => {});
    };
  }, [packageId, definitions]);

  useEffect(() => {
    const own = running.current;
    const server = serverFor(tab);
    if (!own || !packageId || !server || !editorMounted || own.served.has(tab)) return;
    const model = monaco.editor.getModel(monaco.Uri.parse(tab));
    if (!model) return;
    own.served.set(tab, () => {});
    let client = own.clients.get(server);
    if (!client) {
      client = LanguageClient.start(server, packageId);
      own.clients.set(server, client);
    }
    client.then(
      (c) => {
        if (running.current === own) own.served.set(tab, serve(model, c, tab));
      },
      (error: Error) => {
        if (running.current === own) setUnavailable(`Language support unavailable: ${error.message}`);
      },
    );
  }, [packageId, definitions, tab, editorMounted]);

  return unavailable;
}
