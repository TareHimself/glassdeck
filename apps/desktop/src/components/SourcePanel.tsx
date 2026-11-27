import Editor from "@monaco-editor/react";
import * as monaco from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import type { LogLevel, LogSource, ScriptError } from "../bindings";
import { useLanguageServers, type LanguageSupport } from "../lsp/useLanguageServers";
import { languageOf, THEME } from "../monaco";
import { classes } from "./classes";
import styles from "./SourcePanel.module.css";

const LOG_LIMIT = 500;

export type LogLine = { n: number; at: string; source: LogSource; level: LogLevel; text: string };
export type StatusTone = "ok" | "problem" | undefined;

function clock(): string {
  return new Date().toLocaleTimeString("en-GB", { hour12: false });
}

/** The same error again right after itself: an edit that still fails the same way. */
function repeats(last: LogLine | undefined, source: LogSource, level: LogLevel, text: string): boolean {
  return level === "error" && last?.level === "error" && last.source === source && last.text === text;
}

/** The editor's debug log: what the scripts print and the errors that stop them, newest last; an error isn't repeated. */
export function useDebugLog() {
  const [lines, setLines] = useState<LogLine[]>([]);
  const add = (source: LogSource, level: LogLevel, text: string) =>
    setLines((all) => (repeats(all.at(-1), source, level, text) ? all : [...all, { n: (all.at(-1)?.n ?? 0) + 1, at: clock(), source, level, text }].slice(-LOG_LIMIT)));
  return { lines, add, clear: () => setLines([]) };
}

type Props = {
  className?: string;
  tabs: readonly string[];
  tab: string;
  onTab: (name: string) => void;
  files: Record<string, string>;
  onEdit: (name: string, text: string) => void;
  status: { tone: StatusTone; line: string; details: string[] };
  log: LogLine[];
  onClearLog: () => void;
  /** Runs language servers for the files: errors as you type, completion, hover */
  language?: LanguageSupport;
  /** An error the preview ran into, marked at its line */
  runError?: ScriptError | null;
};

/** Marks `error` in its file's editor model, and clears the mark everywhere else. */
function useRunErrorMarker(error: ScriptError | null, tab: string) {
  useEffect(() => {
    for (const model of monaco.editor.getModels()) {
      const ours = error?.file && error.line !== null && model.uri.path.endsWith(`/${error.file}`);
      const line = error?.line ?? 1;
      monaco.editor.setModelMarkers(
        model,
        "preview",
        ours && error ? [{ startLineNumber: line, endLineNumber: line, startColumn: 1, endColumn: model.getLineMaxColumn(Math.min(line, model.getLineCount())), severity: monaco.MarkerSeverity.Error, message: error.message, source: "preview" }] : [],
      );
    }
  }, [error, tab]);
}

/** An editor's source: one tab per file, the code, the check's status, and the debug log. */
export function SourcePanel({ className, tabs, tab, onTab, files, onEdit, status, log, onClearLog, language, runError = null }: Props) {
  const logEnd = useRef<HTMLDivElement>(null);
  const [editorMounted, setEditorMounted] = useState(false);
  const languageUnavailable = useLanguageServers(language, tab, editorMounted);
  useRunErrorMarker(runError, `${tab}:${editorMounted}`);

  useEffect(() => {
    void document.fonts.ready.then(() => monaco.editor.remeasureFonts());
  }, []);

  useEffect(() => {
    void logEnd.current?.scrollIntoView({ block: "nearest" });
  }, [log]);

  return (
    <section aria-label="Source" className={classes(styles.source, className)}>
      <div role="tablist" className={styles.tabs}>
        {tabs.map((name) => (
          <button key={name} type="button" role="tab" aria-selected={name === tab} className={styles.tab} onClick={() => onTab(name)}>
            {name}
          </button>
        ))}
      </div>
      <div className={styles.code}>
        <Editor
          path={tab}
          value={files[tab] ?? ""}
          language={languageOf(tab)}
          theme={THEME}
          onChange={(text) => onEdit(tab, text ?? "")}
          onMount={() => setEditorMounted(true)}
          options={{
            fontFamily: "'JetBrains Mono Variable', 'Cascadia Mono', monospace",
            fontSize: 13,
            lineHeight: 21,
            minimap: { enabled: false },
            scrollBeyondLastLine: false,
            padding: { top: 14, bottom: 14 },
            renderLineHighlight: "none",
            automaticLayout: true,
            lineNumbersMinChars: 4,
            glyphMargin: false,
            folding: false,
            overviewRulerLanes: 0,
            tabSize: 4,
            guides: { indentation: false },
          }}
        />
      </div>
      <div className={styles.status}>
        <div className={styles.statusLine}>
          <span className={styles.dot} data-tone={status.tone} />
          {status.line}
        </div>
        {languageUnavailable && <div className={styles.languageNote}>{languageUnavailable}</div>}
        {status.details.map((line) => (
          <div key={line} className={styles.problem}>
            {line}
          </div>
        ))}
      </div>
      <div className={styles.log} aria-label="Debug log">
        <div className={styles.logHead}>
          <span>Debug log</span>
          <button type="button" className={styles.logClear} disabled={log.length === 0} onClick={onClearLog}>
            Clear
          </button>
        </div>
        <div className={styles.logLines} role="log">
          {log.length === 0 && <div className={styles.logEmpty}>No output</div>}
          {log.map((line) => (
            <div key={line.n} className={styles.logLine} data-level={line.level}>
              <span className={styles.logAt}>{line.at}</span>
              <span className={styles.logSource} data-source={line.source}>
                {line.source}
              </span>
              <span className={styles.logText}>{line.text}</span>
            </div>
          ))}
          <div ref={logEnd} />
        </div>
      </div>
    </section>
  );
}
