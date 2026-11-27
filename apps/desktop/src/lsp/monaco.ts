import * as monaco from "monaco-editor";
import { LUAU, SLANG } from "../languages";
import type { LanguageClient } from "./client";

type Range = { start: { line: number; character: number }; end: { line: number; character: number } };
type Markup = string | { kind?: string; language?: string; value: string };

/** Which client serves which Monaco model, so the providers know where to ask. */
type Served = { client: LanguageClient; file: string; flush: () => void };
const served = new Map<string, Served>();

const CHANGE_SETTLE_MS = 150;

const toMonacoRange = (range: Range): monaco.IRange => ({
  startLineNumber: range.start.line + 1,
  startColumn: range.start.character + 1,
  endLineNumber: range.end.line + 1,
  endColumn: range.end.character + 1,
});

const toLspPosition = (position: monaco.Position) => ({ line: position.lineNumber - 1, character: position.column - 1 });

/** LSP's DiagnosticSeverity (1 error .. 4 hint) as Monaco's. */
const SEVERITY: Record<number, monaco.MarkerSeverity> = {
  1: monaco.MarkerSeverity.Error,
  2: monaco.MarkerSeverity.Warning,
  3: monaco.MarkerSeverity.Info,
  4: monaco.MarkerSeverity.Hint,
};

/** LSP's CompletionItemKind (1 text .. 25 type parameter) as Monaco's. */
const K = monaco.languages.CompletionItemKind;
const KIND: Record<number, monaco.languages.CompletionItemKind> = {
  1: K.Text, 2: K.Method, 3: K.Function, 4: K.Constructor, 5: K.Field, 6: K.Variable, 7: K.Class, 8: K.Interface,
  9: K.Module, 10: K.Property, 11: K.Unit, 12: K.Value, 13: K.Enum, 14: K.Keyword, 15: K.Snippet, 16: K.Color,
  17: K.File, 18: K.Reference, 19: K.Folder, 20: K.EnumMember, 21: K.Constant, 22: K.Struct, 23: K.Event,
  24: K.Operator, 25: K.TypeParameter,
};

function markdown(markup: Markup): monaco.IMarkdownString {
  if (typeof markup === "string") return { value: markup };
  if (markup.language) return { value: `\`\`\`${markup.language}\n${markup.value}\n\`\`\`` };
  return { value: markup.value };
}

/** Sends the model's text to its server, opens it there, and shows the server's errors on it. */
export function serve(model: monaco.editor.ITextModel, client: LanguageClient, file: string): () => void {
  client.open(file, model.getLanguageId(), model.getValue());
  let timer: number | undefined;
  const flush = () => {
    if (timer === undefined) return;
    window.clearTimeout(timer);
    timer = undefined;
    client.change(file, model.getValue());
  };
  const changes = model.onDidChangeContent(() => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => {
      timer = undefined;
      client.change(file, model.getValue());
    }, CHANGE_SETTLE_MS);
  });
  client.onNotification("textDocument/publishDiagnostics", (params) => {
    if (params.uri !== client.uriOf(file) || model.isDisposed()) return;
    const diagnostics = (params.diagnostics as { range: Range; severity?: number; message: string; source?: string }[]) ?? [];
    monaco.editor.setModelMarkers(
      model,
      "lsp",
      diagnostics.map((d) => ({ ...toMonacoRange(d.range), severity: SEVERITY[d.severity ?? 1] ?? monaco.MarkerSeverity.Error, message: d.message, source: d.source })),
    );
  });
  const key = model.uri.toString();
  served.set(key, { client, file, flush });
  return () => {
    window.clearTimeout(timer);
    changes.dispose();
    if (served.get(key)?.client === client) served.delete(key);
    if (!model.isDisposed()) monaco.editor.setModelMarkers(model, "lsp", []);
  };
}

/** The served model's client, with its latest text sent, or nothing for a model no server serves. */
function ready(model: monaco.editor.ITextModel): Served | undefined {
  const entry = served.get(model.uri.toString());
  entry?.flush();
  return entry;
}

const textDocument = (entry: Served) => ({ uri: entry.client.uriOf(entry.file) });

const completion: monaco.languages.CompletionItemProvider = {
  async provideCompletionItems(model, position, context) {
    const entry = ready(model);
    if (!entry) return { suggestions: [] };
    const result = await entry.client.request<unknown>("textDocument/completion", {
      textDocument: textDocument(entry),
      position: toLspPosition(position),
      context: context.triggerCharacter ? { triggerKind: 2, triggerCharacter: context.triggerCharacter } : { triggerKind: 1 },
    });
    const items = (Array.isArray(result) ? result : ((result as { items?: unknown[] } | null)?.items ?? [])) as {
      label: string;
      kind?: number;
      detail?: string;
      documentation?: Markup;
      insertText?: string;
      insertTextFormat?: number;
      sortText?: string;
      filterText?: string;
      textEdit?: { range?: Range; insert?: Range; newText: string };
    }[];
    const word = model.getWordUntilPosition(position);
    const fallback = { startLineNumber: position.lineNumber, startColumn: word.startColumn, endLineNumber: position.lineNumber, endColumn: word.endColumn };
    return {
      suggestions: items.map((item) => ({
        label: item.label,
        kind: KIND[item.kind ?? 1] ?? K.Text,
        detail: item.detail,
        documentation: item.documentation === undefined ? undefined : markdown(item.documentation),
        insertText: item.textEdit?.newText ?? item.insertText ?? item.label,
        insertTextRules: item.insertTextFormat === 2 ? monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet : undefined,
        sortText: item.sortText,
        filterText: item.filterText,
        range: item.textEdit?.range ? toMonacoRange(item.textEdit.range) : item.textEdit?.insert ? toMonacoRange(item.textEdit.insert) : fallback,
      })),
    };
  },
};

const hover: monaco.languages.HoverProvider = {
  async provideHover(model, position) {
    const entry = ready(model);
    if (!entry) return null;
    const result = await entry.client.request<{ contents: Markup | Markup[]; range?: Range } | null>("textDocument/hover", {
      textDocument: textDocument(entry),
      position: toLspPosition(position),
    });
    if (!result) return null;
    const contents = (Array.isArray(result.contents) ? result.contents : [result.contents]).map(markdown).filter((m) => m.value.trim() !== "");
    return contents.length === 0 ? null : { contents, range: result.range ? toMonacoRange(result.range) : undefined };
  },
};

type Signature = { label: string; documentation?: Markup; parameters?: { label: string | [number, number]; documentation?: Markup }[] };

const signatureHelp: monaco.languages.SignatureHelpProvider = {
  signatureHelpTriggerCharacters: ["(", ","],
  async provideSignatureHelp(model, position) {
    const entry = ready(model);
    if (!entry) return null;
    const result = await entry.client.request<{ signatures: Signature[]; activeSignature?: number; activeParameter?: number } | null>("textDocument/signatureHelp", {
      textDocument: textDocument(entry),
      position: toLspPosition(position),
    });
    if (!result || result.signatures.length === 0) return null;
    return {
      value: {
        signatures: result.signatures.map((s) => ({
          label: s.label,
          documentation: s.documentation === undefined ? undefined : markdown(s.documentation),
          parameters: (s.parameters ?? []).map((p) => ({ label: p.label, documentation: p.documentation === undefined ? undefined : markdown(p.documentation) })),
        })),
        activeSignature: result.activeSignature ?? 0,
        activeParameter: result.activeParameter ?? 0,
      },
      dispose: () => {},
    };
  },
};

let registered = false;

/** Completion, hover and signature help for Luau and Slang, answered by whichever server serves the model. */
export function registerLanguageProviders() {
  if (registered) return;
  registered = true;
  for (const [language, triggers] of [
    [LUAU, [".", ":"]],
    [SLANG, ["."]],
  ] as const) {
    monaco.languages.registerCompletionItemProvider(language, { ...completion, triggerCharacters: [...triggers] });
    monaco.languages.registerHoverProvider(language, hover);
    monaco.languages.registerSignatureHelpProvider(language, signatureHelp);
  }
}
