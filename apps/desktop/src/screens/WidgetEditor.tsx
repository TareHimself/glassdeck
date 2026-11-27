import Editor from "@monaco-editor/react";
import * as monaco from "monaco-editor";
import { useEffect, useMemo, useRef, useState } from "react";
import { useParams } from "react-router";
import { commands, events, WIDGET_AREA, type Dims, type LibraryError, type LogLevel, type LogSource, type OptionsPanel, type Sizing, type SourceFile, type Value, type WidgetSource } from "../bindings";
import { Button } from "../components/Button";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";
import { LANGUAGE, THEME } from "../monaco";
import { OptionControls, type Chosen } from "./OptionControls";
import styles from "./WidgetEditor.module.css";

const BACK = { to: "/widgets", label: "Widgets" };
const SCRIPT_ORDER = ["keyboard.luau", "options.luau", "host.luau"];
const OPTIONS_SCRIPT = "options.luau";
const PREVIEW_ROOM = { w: 300, h: 200 };
const EDIT_SETTLE_MS = 400;

type Files = Record<string, string>;
type Loaded = { state: "loading" } | { state: "ready"; source: WidgetSource } | { state: "failed"; error: LibraryError };
type Check = { state: "checking" } | { state: "done"; problems: string[]; files: number } | { state: "unavailable"; reason: string };
type Frame = { state: "none" } | { state: "frame"; png: string };
type LogLine = { n: number; at: string; source: LogSource; level: LogLevel; text: string };

const LOG_LIMIT = 500;

function clock(): string {
  return new Date().toLocaleTimeString("en-GB", { hour12: false });
}

function filesOf(list: SourceFile[]): Files {
  return Object.fromEntries(list.map((f) => [f.name, f.text]));
}

function listOf(files: Files): SourceFile[] {
  return SCRIPT_ORDER.filter((name) => name in files).map((name) => ({ name, text: files[name] ?? "" }));
}

function sizesOf(sizing: Sizing): { name: string; dims: Dims }[] {
  switch (sizing.kind) {
    case "fixed":
      return [{ name: "Size", dims: sizing.size }];
    case "range":
      return [
        { name: "Min", dims: sizing.min },
        { name: "Default", dims: sizing.size },
        { name: "Max", dims: sizing.max },
      ];
    case "any":
      return [
        { name: "Default", dims: sizing.size },
        { name: "Max", dims: WIDGET_AREA },
      ];
  }
}

function checkLine(check: Check): string {
  switch (check.state) {
    case "checking":
      return "luau-check: checking…";
    case "unavailable":
      return `luau-check didn't run: ${check.reason}`;
    case "done":
      if (check.problems.length > 0) return `luau-check: ${check.problems.length} ${check.problems.length === 1 ? "problem" : "problems"}`;
      return `luau-check: no problems in ${check.files} ${check.files === 1 ? "file" : "files"}`;
  }
}

export function WidgetEditor() {
  const { id = "" } = useParams();
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [files, setFiles] = useState<Files>({});
  const [saved, setSaved] = useState<{ files: Files; title: string; version: string }>({ files: {}, title: "", version: "" });
  const [title, setTitle] = useState("");
  const [version, setVersion] = useState("");
  const [tab, setTab] = useState("keyboard.luau");
  const [check, setCheck] = useState<Check>({ state: "checking" });
  const [chosen, setChosen] = useState<Chosen>({});
  const [panel, setPanel] = useState<OptionsPanel | null>(null);
  const [sizeIndex, setSizeIndex] = useState(0);
  const [frame, setFrame] = useState<Frame>({ state: "none" });
  const [log, setLog] = useState<LogLine[]>([]);
  const logEnd = useRef<HTMLDivElement>(null);
  const addLog = (source: LogSource, level: LogLevel, text: string) =>
    setLog((lines) => [...lines, { n: (lines.at(-1)?.n ?? 0) + 1, at: clock(), source, level, text }].slice(-LOG_LIMIT));
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const started = useRef(false);

  const runCheck = async () => {
    setCheck({ state: "checking" });
    const result = await commands.checkWidget(id);
    if (result.status === "ok") setCheck({ state: "done", problems: result.data.problems, files: result.data.checked });
    else setCheck({ state: "unavailable", reason: describeLibraryError(result.error) });
  };

  useEffect(() => {
    void commands.readWidget(id).then((result) => {
      if (result.status === "error") return setLoaded({ state: "failed", error: result.error });
      const source = result.data;
      const initial = filesOf(source.files);
      setLoaded({ state: "ready", source });
      setFiles(initial);
      setSaved({ files: initial, title: source.widget.title, version: source.widget.version });
      setTitle(source.widget.title);
      setVersion(source.widget.version);
      setSizeIndex(source.widget.sizing.kind === "range" ? 1 : 0);
      void commands.checkWidget(id).then((checked) =>
        setCheck(
          checked.status === "ok"
            ? { state: "done", problems: checked.data.problems, files: checked.data.checked }
            : { state: "unavailable", reason: describeLibraryError(checked.error) },
        ),
      );
    });
  }, [id]);

  useEffect(() => {
    const listening = events.previewUpdate.listen(({ payload }) => {
      switch (payload.state) {
        case "frame":
          return setFrame({ state: "frame", png: payload.png });
        case "cleared":
          return setFrame({ state: "none" });
        case "log":
          return addLog(payload.source, payload.level, payload.text);
      }
    });
    return () => {
      void listening.then((unlisten) => unlisten());
      void commands.stopPreview();
    };
  }, []);

  useEffect(() => {
    void document.fonts.ready.then(() => monaco.editor.remeasureFonts());
  }, []);

  const sizes = loaded.state === "ready" ? sizesOf(loaded.source.widget.sizing) : [];
  const size = sizes[sizeIndex]?.dims;
  const filesKey = JSON.stringify(files);

  useEffect(() => {
    if (loaded.state !== "ready" || !size) return;
    const timer = setTimeout(
      () => {
        started.current = true;
        void commands.previewWidget(id, listOf(files), size, chosen).then((result) => {
          if (result.status === "error") addLog("pack", "error", describeLibraryError(result.error));
        });
      },
      started.current ? EDIT_SETTLE_MS : 0,
    );
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loaded.state, filesKey]);

  useEffect(() => {
    if (started.current && size) void commands.previewResize(size);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sizeIndex]);

  const optionsSource = files[OPTIONS_SCRIPT];
  const chosenKey = JSON.stringify(chosen);
  const lastOptionsSource = useRef(optionsSource);
  useEffect(() => {
    const edited = lastOptionsSource.current !== optionsSource;
    lastOptionsSource.current = optionsSource;
    const timer = setTimeout(() => void commands.widgetOptions(optionsSource ?? "", chosen).then(setPanel), edited ? EDIT_SETTLE_MS : 0);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [optionsSource, chosenKey]);

  useEffect(() => {
    void logEnd.current?.scrollIntoView({ block: "nearest" });
  }, [log]);

  const dirty = useMemo(
    () => title !== saved.title || version !== saved.version || JSON.stringify(files) !== JSON.stringify(saved.files),
    [title, version, files, saved],
  );

  const save = async (): Promise<boolean> => {
    const result = await commands.saveWidget(id, { title, version, files: listOf(files) });
    if (result.status === "error") {
      setProblem(result.error);
      return false;
    }
    setProblem(null);
    setSaved({ files, title: result.data.title, version: result.data.version });
    setLoaded((l) => (l.state === "ready" ? { state: "ready", source: { ...l.source, widget: result.data } } : l));
    await runCheck();
    return true;
  };

  const pack = async () => {
    if (dirty && !(await save())) return;
    const result = await commands.exportPackage(id);
    if (result.status === "error") setProblem(result.error);
  };

  const choose = (path: string, value: Value) => {
    const next = { ...chosen, [path]: value };
    setChosen(next);
    void commands.previewOptions(next);
  };

  if (loaded.state === "loading") return <PageHeader back={BACK} title="" />;
  if (loaded.state === "failed")
    return (
      <div className={styles.page}>
        <PageHeader back={BACK} title="" />
        <Problem>{describeLibraryError(loaded.error)}</Problem>
      </div>
    );

  const { widget } = loaded.source;
  const scale = size ? Math.min(1, PREVIEW_ROOM.w / size.w, PREVIEW_ROOM.h / size.h) : 1;
  const tabs = SCRIPT_ORDER;
  const tone = check.state === "done" ? (check.problems.length > 0 ? "problem" : "ok") : check.state === "unavailable" ? "problem" : undefined;

  return (
    <div className={styles.page}>
      <PageHeader
        back={BACK}
        title={widget.title}
        id={`${widget.id} ${widget.version}`}
        actions={
          <>
            <Button onClick={() => void runCheck()}>Check</Button>
            <Button onClick={() => void pack()}>Pack .deck</Button>
            <Button variant="primary" onClick={() => void (dirty && save())}>
              {dirty ? "Save" : "Saved"}
            </Button>
          </>
        }
      />

      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <div className={styles.columns}>
        <section aria-label="Source" className={styles.source}>
          <div role="tablist" className={styles.tabs}>
            {tabs.map((name) => (
              <button key={name} type="button" role="tab" aria-selected={name === tab} className={styles.tab} onClick={() => setTab(name)}>
                {name}
              </button>
            ))}
          </div>
          <div className={styles.code}>
            <Editor
              path={tab}
              value={files[tab] ?? ""}
              language={LANGUAGE}
              theme={THEME}
              onChange={(text) => setFiles((f) => ({ ...f, [tab]: text ?? "" }))}
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
              <span className={styles.dot} data-tone={tone} />
              {checkLine(check)}
            </div>
            {check.state === "done" &&
              check.problems.map((line) => (
                <div key={line} className={styles.problem}>
                  {line}
                </div>
              ))}
          </div>
          <div className={styles.log} aria-label="Debug log">
            <div className={styles.logHead}>
              <span>Debug log</span>
              <button type="button" className={styles.logClear} disabled={log.length === 0} onClick={() => setLog([])}>
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

        <aside className={styles.aside}>
          <section aria-label="Preview" className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Preview</h2>
              {size && (
                <span className={styles.note}>
                  {size.w} × {size.h}
                  {scale < 1 ? `  at ${Math.round(scale * 100)}%` : ""}
                </span>
              )}
            </div>
            <div className={styles.stage}>
              {size && (
                <div className={styles.box} style={{ width: size.w * scale, height: size.h * scale }}>
                  {frame.state === "frame" && <img alt={`${widget.title} preview`} src={`data:image/png;base64,${frame.png}`} />}
                </div>
              )}
            </div>
            <div role="group" aria-label="Size" className={styles.sizes}>
              {sizes.map((s, i) => (
                <button key={s.name} type="button" className={styles.size} aria-pressed={i === sizeIndex} onClick={() => setSizeIndex(i)}>
                  <span className={styles.sizeName}>{s.name}</span>
                  <span className={styles.sizeDims}>
                    {s.dims.w}×{s.dims.h}
                  </span>
                </button>
              ))}
            </div>
          </section>

          <section aria-label="Details" className={styles.details}>
            <label htmlFor="we-title">Title</label>
            <label htmlFor="we-version">Version</label>
            <input id="we-title" type="text" value={title} onChange={(e) => setTitle(e.target.value)} />
            <input id="we-version" type="text" data-mono value={version} spellCheck={false} onChange={(e) => setVersion(e.target.value)} />
          </section>

          {panel && !(panel.state === "ready" && panel.items.length === 0) && (
            <section aria-label="Options" className={styles.options}>
              <div className={styles.cardHead}>
                <h2 className={styles.cardTitle}>Options</h2>
                <span className={styles.note}>from options.luau</span>
              </div>
              {panel.state === "ready" ? <OptionControls items={panel.items} chosen={chosen} onChange={choose} /> : <div className={styles.failed}>{panel.reason}</div>}
            </section>
          )}
        </aside>
      </div>
    </div>
  );
}
