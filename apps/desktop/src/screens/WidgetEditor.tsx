import { useEffect, useMemo, useRef, useState } from "react";
import { useParams } from "react-router";
import { commands, events, WIDGET_AREA, type Dims, type LibraryError, type OptionsPanel, type Sizing, type SourceFile, type Value, type WidgetSource } from "../bindings";
import { Button } from "../components/Button";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { SourcePanel, useDebugLog, type StatusTone } from "../components/SourcePanel";
import { describeLibraryError } from "../library";
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
  const log = useDebugLog();
  const addLog = log.add;
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
  const tone: StatusTone = check.state === "done" ? (check.problems.length > 0 ? "problem" : "ok") : check.state === "unavailable" ? "problem" : undefined;

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
        <SourcePanel
          className={styles.source}
          tabs={SCRIPT_ORDER}
          tab={tab}
          onTab={setTab}
          files={files}
          onEdit={(name, text) => setFiles((f) => ({ ...f, [name]: text }))}
          status={{ tone, line: checkLine(check), details: check.state === "done" ? check.problems : [] }}
          log={log.lines}
          onClearLog={log.clear}
        />

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
