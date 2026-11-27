import { loader } from "@monaco-editor/react";
import * as monaco from "monaco-editor";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";

self.MonacoEnvironment = { getWorker: () => new EditorWorker() };
loader.config({ monaco });

export const THEME = "glassdeck";

monaco.editor.defineTheme(THEME, {
  base: "vs-dark",
  inherit: true,
  rules: [
    { token: "", foreground: "d6dce6" },
    { token: "comment", foreground: "6f7a8b" },
    { token: "keyword", foreground: "c79bff" },
    { token: "string", foreground: "9fe0a8" },
    { token: "number", foreground: "f5b678" },
    { token: "type", foreground: "6fd3e0" },
    { token: "delimiter", foreground: "98a2b3" },
    { token: "identifier", foreground: "d6dce6" },
  ],
  colors: {
    "editor.background": "#12151b",
    "editor.foreground": "#d6dce6",
    "editorLineNumber.foreground": "#566071",
    "editorLineNumber.activeForeground": "#a1abb9",
    "editor.lineHighlightBackground": "#161a2100",
    "editor.lineHighlightBorder": "#161a2100",
    "editorGutter.background": "#12151b",
  },
});

/** Monaco has no Luau or Slang grammar: Luau reads well as Lua, and Slang as C++. */
export function languageOf(file: string): string {
  return file.endsWith(".slang") ? "cpp" : "lua";
}
