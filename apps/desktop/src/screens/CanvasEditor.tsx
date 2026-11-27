import { useEffect, useMemo, useRef, useState } from "react";
import { commands, type BackgroundSummary, type CanvasCheck, type InputReading, type LibraryError, type OptionsPanel, type PanelLayout, type PanelRect, type SourceFile, type Value } from "../bindings";
import { useLiveCanvas } from "../previews";
import { Button } from "../components/Button";
import { MESSAGES_SCRIPT, useMessageTypes } from "../messageTypes";
import { RunNote } from "../components/RunNote";
import { errorLine, markedError, RUNNING, type RunStatus } from "../runStatus";
import { PageHeader, useBack } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { SourcePanel, useDebugLog, type StatusTone } from "../components/SourcePanel";
import { describeLibraryError } from "../library";
import { OptionControls, type Chosen } from "./OptionControls";
import styles from "./CanvasEditor.module.css";

const LIBRARY = { to: "/backgrounds", label: "Backgrounds" };
const FILES = ["canvas.slang", "keyboard.luau", "options.luau", "host.luau", "messages.luau"];
const OPTIONS_SCRIPT = "options.luau";
const PANEL = { w: 1920, h: 1080 };
const EDIT_SETTLE_MS = 400;
/** How long the button says the preview was set */
const PREVIEW_SET_SHOWN_MS = 1500;

type Files = Record<string, string>;
type Check = { state: "checking" } | { state: "done"; result: CanvasCheck } | { state: "unavailable"; reason: string };
type Frame = { state: "none" } | { state: "frame"; time: number };

function filesOf(list: SourceFile[]): Files {
  return Object.fromEntries(list.map((f) => [f.name, f.text]));
}

function listOf(files: Files): SourceFile[] {
  return FILES.map((name) => ({ name, text: files[name] ?? "" }));
}

function statusOf(check: Check): { tone: StatusTone; line: string; details: string[] } {
  switch (check.state) {
    case "checking":
      return { tone: undefined, line: "Checking…", details: [] };
    case "unavailable":
      return { tone: "problem", line: `The checks didn't run: ${check.reason}`, details: [] };
    case "done": {
      const { shader_error, problems } = check.result;
      if (shader_error) return { tone: "problem", line: "Slang didn't compile", details: [shader_error] };
      if (problems.length > 0) return { tone: "problem", line: `Slang compiled, luau-check: ${problems.length} ${problems.length === 1 ? "problem" : "problems"}`, details: problems };
      return { tone: "ok", line: "Slang compiled, luau-check: no problems", details: [] };
    }
  }
}

/** A rectangle on the panel as percentages of the preview, which shows the whole panel. */
function placed(rect: PanelRect) {
  const percent = (value: number | null, of: number) => `${((value ?? 0) / of) * 100}%`;
  return { left: percent(rect.x, PANEL.w), top: percent(rect.y, PANEL.h), width: percent(rect.w, PANEL.w), height: percent(rect.h, PANEL.h) };
}

/** Floats come through as null when they aren't numbers, such as NaN. */
type Floats = (number | null)[];

function hexOf(values: Floats): string {
  const channel = (v: number | null) => Math.round(Math.min(1, Math.max(0, v ?? 0)) * 255).toString(16).padStart(2, "0");
  return `#${values.slice(0, 3).map(channel).join("")}`;
}

function numbersOf(values: Floats): string {
  const shown = values.map((v) => (v === null ? "NaN" : v.toFixed(3)));
  return shown.length === 1 ? (shown[0] ?? "") : `(${shown.join(", ")})`;
}

/** Three or four components read as a color, as the canvases write them with `color.hex`; anything else as numbers. */
function isColor(input: InputReading): boolean {
  return input.values.length === 3 || input.values.length === 4;
}

function PlayIcon({ playing }: { playing: boolean }) {
  return playing ? (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
      <rect x="3" y="2" width="3.5" height="12" rx="1" />
      <rect x="9.5" y="2" width="3.5" height="12" rx="1" />
    </svg>
  ) : (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
      <path d="M4 2.5v11l9-5.5z" />
    </svg>
  );
}

export function CanvasEditor({ id }: { id: string }) {
  const back = useBack(LIBRARY);
  const [background, setBackground] = useState<BackgroundSummary | null>(null);
  const [failed, setFailed] = useState<LibraryError | null>(null);
  const [files, setFiles] = useState<Files>({});
  const [saved, setSaved] = useState<{ files: Files; title: string; version: string }>({ files: {}, title: "", version: "" });
  const [title, setTitle] = useState("");
  const [version, setVersion] = useState("");
  const [tab, setTab] = useState("canvas.slang");
  const [check, setCheck] = useState<Check>({ state: "checking" });
  const messageTypes = useMessageTypes(id, files, Object.keys(files).length > 0);
  const definitions = useMemo(() => (check.state === "checking" ? null : { check, messages: messageTypes.version }), [check, messageTypes.version]);
  const messageProblems = useMemo(() => ({ file: MESSAGES_SCRIPT, items: messageTypes.problems }), [messageTypes.problems]);
  const [frame, setFrame] = useState<Frame>({ state: "none" });
  const [runStatus, setRunStatus] = useState<RunStatus>(RUNNING);
  const [inputs, setInputs] = useState<InputReading[]>([]);
  const [playing, setPlaying] = useState(true);
  const [showKeys, setShowKeys] = useState(true);
  const [showArea, setShowArea] = useState(false);
  const [layout, setLayout] = useState<PanelLayout | null>(null);
  const [chosen, setChosen] = useState<Chosen>({});
  const [panel, setPanel] = useState<OptionsPanel | null>(null);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const log = useDebugLog();
  const started = useRef(false);

  const runCheck = async () => {
    setCheck({ state: "checking" });
    const result = await commands.checkCanvas(id);
    setCheck(result.status === "ok" ? { state: "done", result: result.data } : { state: "unavailable", reason: describeLibraryError(result.error) });
  };

  useEffect(() => {
    void commands.panelLayout().then(setLayout);
    void commands.readCanvas(id).then((result) => {
      if (result.status === "error") return setFailed(result.error);
      const initial = filesOf(result.data.files);
      setBackground(result.data.background);
      setFiles(initial);
      setSaved({ files: initial, title: result.data.background.title, version: result.data.background.version });
      setTitle(result.data.background.title);
      setVersion(result.data.background.version);
      void runCheck();
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const [previewSet, setPreviewSet] = useState(false);
  useEffect(() => {
    if (!previewSet) return;
    const timer = setTimeout(() => setPreviewSet(false), PREVIEW_SET_SHOWN_MS);
    return () => clearTimeout(timer);
  }, [previewSet]);

  /** Makes this moment the canvas's still, on its cards and in its .deck. */
  const choosePreviewFrame = async (at: number) => {
    const set = await commands.setPreviewTime(id, at);
    if (set.status === "error") return setProblem(set.error);
    setPreviewSet(true);
  };
  const live = useLiveCanvas((change) => {
    switch (change.state) {
      case "frame":
        return setFrame({ state: "frame", time: change.time ?? 0 });
      case "cleared":
        return setFrame({ state: "none" });
      case "inputs":
        return setInputs(change.inputs);
      case "log":
        return log.add(change.source, change.level, change.text);
      case "held":
        if (!change.error.syntax) log.add("keyboard", "error", errorLine(change.error));
        return setRunStatus({ state: "held", error: change.error });
      case "failed":
        return setRunStatus({ state: "failed", error: change.error });
    }
  });

  const filesKey = JSON.stringify(files);
  useEffect(() => {
    if (!background) return;
    const timer = setTimeout(
      () => {
        started.current = true;
        setRunStatus(RUNNING);
        live.show(listOf(files), chosen);
      },
      started.current ? EDIT_SETTLE_MS : 0,
    );
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [background === null, filesKey]);

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
    const result = await commands.saveCanvas(id, { title, version, files: listOf(files) });
    if (result.status === "error") {
      setProblem(result.error);
      return false;
    }
    setProblem(null);
    setBackground(result.data);
    setSaved({ files, title: result.data.title, version: result.data.version });
    await runCheck();
    return true;
  };

  const pack = async () => {
    if (dirty && !(await save())) return;
    const result = await commands.exportPackage(id);
    if (result.status === "error") setProblem(result.error);
  };

  const togglePlay = () => {
    setPlaying(!playing);
    void commands.playCanvas(!playing);
  };

  const choose = (path: string, value: Value) => {
    const next = { ...chosen, [path]: value };
    setChosen(next);
    void commands.canvasOptions(next);
  };

  if (failed)
    return (
      <div className={styles.page}>
        <PageHeader back={back} title="" />
        <Problem>{describeLibraryError(failed)}</Problem>
      </div>
    );
  if (!background) return <PageHeader back={back} title="" />;

  const colors = inputs.filter(isColor);
  const numbers = inputs.filter((input) => !isColor(input));
  const hasOptions = panel !== null && !(panel.state === "ready" && panel.items.length === 0);

  return (
    <div className={styles.page}>
      <PageHeader
        back={back}
        title={background.title}
        id={`${background.id} ${background.version}, canvas background`}
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
          tabs={FILES}
          tab={tab}
          onTab={setTab}
          files={files}
          onEdit={(name, text) => setFiles((f) => ({ ...f, [name]: text }))}
          status={statusOf(check)}
          log={log.lines}
          onClearLog={log.clear}
          language={{ packageId: id, definitions }}
          problems={messageProblems}
          runError={markedError(runStatus)}
        />

        <div className={styles.side}>
          <section aria-label="Preview" className={styles.preview}>
            <div className={styles.stage}>
              <canvas ref={live.ref} className={styles.frame} aria-label={`${background.title} preview`} data-hidden={frame.state !== "frame" || undefined} />
              {showArea && layout && <div className={styles.area} style={placed(layout.widget_area)} />}
              {showKeys && layout?.keys.map((key, i) => <div key={i} className={styles.key} style={placed(key)} />)}
              <RunNote status={runStatus} />
            </div>
            <div className={styles.controls}>
              <button type="button" className={styles.play} aria-label={playing ? "Pause" : "Play"} onClick={togglePlay}>
                <PlayIcon playing={playing} />
              </button>
              <button type="button" className={styles.control} onClick={() => void commands.restartCanvas()}>
                Restart
              </button>
              <span className={styles.time}>frame.time {frame.state === "frame" ? frame.time.toFixed(2) : "0.00"} s</span>
              <button type="button" className={styles.control} disabled={frame.state !== "frame"} onClick={() => frame.state === "frame" && void choosePreviewFrame(frame.time)}>
                {previewSet ? "Preview set" : "Use this frame"}
              </button>
              <div className={styles.spacer} />
              <button type="button" className={styles.control} aria-pressed={showKeys} onClick={() => setShowKeys(!showKeys)}>
                Keys
              </button>
              <button type="button" className={styles.control} aria-pressed={showArea} onClick={() => setShowArea(!showArea)}>
                Widget area
              </button>
            </div>
          </section>

          <section aria-label="Inputs" className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Inputs, live</h2>
              <span className={styles.note}>set by keyboard.luau</span>
            </div>
            <div className={styles.inputs}>
              {colors.map((input) => (
                <div key={input.name} className={styles.input}>
                  <span className={styles.swatch} style={{ background: hexOf(input.values) }} />
                  <span className={styles.inputName}>{input.name}</span>
                  <span className={styles.muted}>{hexOf(input.values)}</span>
                </div>
              ))}
              {numbers.map((input) => (
                <div key={input.name} className={styles.input}>
                  <span className={styles.inputName}>{input.name}</span>
                  <span className={styles.number}>{numbersOf(input.values)}</span>
                </div>
              ))}
            </div>
          </section>

          {hasOptions && (
            <section aria-label="Options" className={styles.card}>
              <div className={styles.cardHead}>
                <h2 className={styles.cardTitle}>Options</h2>
                <span className={styles.note}>from options.luau</span>
              </div>
              {panel.state === "ready" ? <OptionControls items={panel.items} chosen={chosen} onChange={choose} /> : <div className={styles.failed}>{panel.reason}</div>}
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
