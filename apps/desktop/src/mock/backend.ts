import { emit } from "@tauri-apps/api/event";
import { mockConvertFileSrc, mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { BUILT_IN_BACKGROUND } from "../bindings";
import type {
  BackgroundSummary,
  BackupSummary,
  Keyboard,
  SetupStep,
  SetupUpdate,
  Software,
  Dims,
  GlassdeckdUpdate,
  KeyboardLink,
  LibraryError,
  NewBackground,
  NewWidget,
  OptionsPanel,
  PlacedSource,
  SourceFile,
  Theme,
  WidgetSummary,
} from "../bindings";
import { BACKGROUNDS, CANVAS_FILES, CLOCK_KEYBOARD, CLOCK_OPTIONS, PANEL_LAYOUT, STATUS, STREAMS, THEME, WIDGETS } from "./fixtures";
import { canvasFrame, monitorFrame, pictureFrame, staleFrame, widgetTile } from "./frames";

const STALE_FRAME = staleFrame();
/** How long the mock takes to fit a picked picture, as a big photo might. */
const PICTURE_LOAD_MS = 600;

/** A canvas's shader the way Slang names things (`frame_0.time_0`), drifting colour over time. */
const MOCK_SHADER = `#version 300 es
precision highp float;
struct Frame_0 { float time_0; vec2 resolution_0; };
uniform Frame_0 frame_0;
out vec4 color;
void main() {
    vec2 uv = gl_FragCoord.xy / frame_0.resolution_0;
    color = vec4(0.45 + 0.35 * sin(frame_0.time_0 + uv.x * 4.0), 0.25 + 0.5 * uv.y, 0.75, 1.0);
}
`;

/** All black: what a late shader from a replaced run would draw, if it ever got through. */
const STALE_SHADER = `#version 300 es
precision highp float;
out vec4 color;
void main() { color = vec4(0.0, 0.0, 0.0, 1.0); }
`;

/** The app's native side, faked in memory for running the frontend in a plain browser. */
const state = {
  widgets: structuredClone(WIDGETS),
  backgrounds: structuredClone(BACKGROUNDS),
  theme: structuredClone(THEME),
  status: structuredClone(STATUS),
  canvas: null as { id: string; run: number; started: number; timer: number } | null,
  nextLspSession: 1,
};

type Args = Record<string, unknown>;

const notFound = (id: string): LibraryError => ({ kind: "not_found", id });

function widget(id: string): WidgetSummary {
  const found = state.widgets.find((w) => w.id === id);
  if (!found) throw notFound(id);
  return found;
}

function background(id: string): BackgroundSummary {
  const found = state.backgrounds.find((b) => b.id === id);
  if (!found) throw notFound(id);
  return found;
}

const glassdeckd = (update: GlassdeckdUpdate) => void emit("glassdeckd-update", update);

const MOCK_SERIAL = "E4645852431F3223";
const BACKUPS_FOLDER = "C:\\Users\\you\\AppData\\Roaming\\Glassdeck\\Keyboard backups";

/** A checked backup of the mock keyboard, saved as glassdeck-setup's archive would name it. */
function mockBackup(folder: string, created: number): BackupSummary {
  return { serial: MOCK_SERIAL, file: `${folder}\\${MOCK_SERIAL} 2026-11-27 21.04.zip`, created, firmware: "1.4.9.D", display: "1.4.19", bytes: 148_570_911 };
}

/** The plugged-in keyboard; a test picks Flux software with `localStorage["glassdeck.mock.keyboard"] = "flux"` before loading. */
const keyboard = {
  software: ((localStorage.getItem("glassdeck.mock.keyboard") as Software | null) ?? "glassdeck") satisfies Software,
  driverMissing: localStorage.getItem("glassdeck.mock.driver") !== "ready",
  backups: [] as BackupSummary[],
  running: false,
};

function mockKeyboard(): Keyboard {
  if (keyboard.software === "absent") return { software: "absent", serial: null, firmware: null, computer: null };
  return {
    software: keyboard.software,
    serial: MOCK_SERIAL,
    firmware: keyboard.software === "flux" ? "1.4.9.D" : "glassdeck-scanner 0.0.1",
    computer: { bridge: "1.4.8", display: "1.4.19", os: "0.0.9", free_bytes: 5_900_000_000 },
  };
}

/** A backup run as glassdeck-setup reports it, a step every few hundred milliseconds. */
function runBackup(folder: string) {
  keyboard.running = true;
  const send = (update: SetupUpdate) => void emit("setup-update", update);
  const plan: (() => boolean)[] = [];
  const step = (s: SetupStep, notes: string[] = []) => {
    plan.push(() => (send({ state: "started", step: s }), true));
    for (const text of notes) plan.push(() => (send({ state: "note", step: s, text, progress: true }), true));
    plan.push(() => (send({ state: "finished", step: s }), true));
  };
  step("scanner_found");
  plan.push(() => {
    send({ state: "started", step: "scanner_driver" });
    if (!keyboard.driverMissing) return true;
    keyboard.driverMissing = false;
    send({ state: "needs_you", step: "scanner_driver", todo: ["In Zadig, install WinUSB on RP2 Boot (Interface 1)", "Run setup again"] });
    return false;
  });
  plan.push(() => (send({ state: "finished", step: "scanner_driver" }), true));
  step("scanner_backup", ["reading flash: 4.00 / 16.00 MiB", "reading flash: 8.00 / 16.00 MiB", "reading flash: 12.00 / 16.00 MiB", "reading flash: 16.00 / 16.00 MiB"]);
  step("bridge_found");
  step("probe");
  step("linux_backup");
  plan.push(() => {
    const saved = mockBackup(folder, Math.floor(Date.now() / 1000));
    keyboard.backups = [saved, ...keyboard.backups];
    send({ state: "backed_up", file: saved.file });
    return true;
  });
  const next = (i: number) => {
    const go = plan[i];
    if (!go || !go()) {
      keyboard.running = false;
      send({ state: "ended" });
      return;
    }
    window.setTimeout(() => next(i + 1), 120);
  };
  window.setTimeout(() => next(0), 50);
}

/**
 * Like the real preview, a frame from the run being replaced can still be on its way: the mock sends one,
 * all black (`STALE_FRAME`), shortly after each new start, which the frontend must drop.
 */
function startCanvas(id: string, run: number) {
  const replaced = state.canvas?.run;
  stopCanvas();
  const send = (forRun: number, change: object) => void emit("canvas-update", { run: forRun, change });
  if (replaced !== undefined) {
    window.setTimeout(() => {
      send(replaced, { state: "program", fragment: STALE_SHADER, width: 64, height: 36, passes: [], targets: [] });
      send(replaced, { state: "frame", time: 0, delta: 0, index: 0, inputs: [] });
    }, 150);
  }
  send(run, { state: "program", fragment: MOCK_SHADER, width: 480, height: 270, passes: [], targets: [] });
  const started = performance.now();
  let index = 0;
  let last = 0;
  const timer = window.setInterval(() => {
    const time = (performance.now() - started) / 1000;
    const inputs = mockInputs(time);
    send(run, { state: "frame", time, delta: time - last, index: index++, inputs });
    if (index % 10 === 0) send(run, { state: "inputs", inputs: inputs.map(({ name, value }) => ({ name, values: value.values })) });
    last = time;
  }, 33);
  state.canvas = { id, run, started, timer };
}

/** A colour, a vector and an array, as a canvas's keyboard.luau might write them. */
function mockInputs(time: number) {
  const levels = Array.from({ length: 24 }, (_, i) => Math.abs(Math.sin(time + i * 0.45)) * (1 - i / 30));
  return [
    { name: "night", value: { kind: "float", values: [0.06, 0.04, 0.16] } },
    { name: "roseAt", value: { kind: "float", values: [0.5 + 0.2 * Math.sin(time), 0.3] } },
    { name: "levels", value: { kind: "array", values: levels } },
  ];
}

function stopCanvas() {
  if (state.canvas) window.clearInterval(state.canvas.timer);
  state.canvas = null;
}

/** Each placed widget's first tile, the way the layout preview sends them. */
function showLayout(widgets: PlacedSource[], run: number) {
  widgets.forEach((placed, index) => {
    const found = state.widgets.find((w) => w.id === placed.package);
    const update = found
      ? { state: "tile", index, width: placed.size.w, height: placed.size.h, png: widgetTile(found.title, placed.size.w, placed.size.h, 0) }
      : { state: "stopped", index, reason: `${placed.package} isn't in the library` };
    void emit("layout-update", { run, change: update });
  });
}

/**
 * What the preview makes of an edit, as the real one would: a line with `@@` doesn't parse, and one
 * that indexes `nil.` fails when it runs; the first is held, the second stops the run.
 */
function scriptProblem(files: SourceFile[]): { held: boolean; error: { file: string; line: number; message: string; syntax: boolean } } | null {
  for (const file of files) {
    const lines = file.text.split("\n");
    const syntax = lines.findIndex((line) => line.includes("@@"));
    if (syntax >= 0) return { held: true, error: { file: file.name, line: syntax + 1, message: "Incomplete statement: expected assignment or a function call", syntax: true } };
    const runtime = lines.findIndex((line) => line.includes("nil."));
    if (runtime >= 0) return { held: false, error: { file: file.name, line: runtime + 1, message: "attempt to index nil", syntax: false } };
  }
  return null;
}

function showWidget(id: string, files: SourceFile[], size: Dims, run: number) {
  const problem = scriptProblem(files);
  if (problem?.held) return void emit("preview-update", { run, change: { state: "held", error: problem.error } });
  void emit("preview-update", { run, change: { state: "frame", width: size.w, height: size.h, png: widgetTile(widget(id).title, size.w, size.h, 0) } });
  if (problem) {
    void emit("preview-update", { run, change: { state: "log", source: "keyboard", level: "error", text: `running ${problem.error.file}: runtime error: line ${problem.error.line}: ${problem.error.message}` } });
    void emit("preview-update", { run, change: { state: "failed", error: problem.error } });
  }
}

function widgetFiles(id: string): SourceFile[] {
  const clock = id.endsWith(".clock");
  return [
    { name: "keyboard.luau", text: clock ? CLOCK_KEYBOARD : "" },
    { name: "options.luau", text: clock ? CLOCK_OPTIONS : "" },
    { name: "host.luau", text: "" },
    { name: "bridge.luau", text: "" },
  ];
}

function optionsPanel(source: string, values: Args): OptionsPanel {
  if (!source.trim()) return { state: "ready", items: [] };
  const value = values.seconds ?? true;
  return {
    state: "ready",
    items: [{ item: "option", path: "seconds", label: "Show seconds", kind: { type: "toggle", default: true }, value: value as boolean, visible: true, hint: null }],
  };
}

/** glassdeckd's WHEP sessions, played by a peer in this page sending a drawn monitor. */
const watching = {
  next: 0,
  peers: new Map<number, { peer: RTCPeerConnection; timer: number }>(),
  async answer(session: number, offer: string): Promise<string> {
    const canvas = document.createElement("canvas");
    [canvas.width, canvas.height] = [480, 270];
    const g = canvas.getContext("2d");
    if (!g) throw new Error("no 2D context");
    const timer = window.setInterval(() => {
      g.fillStyle = "#14203a";
      g.fillRect(0, 0, 480, 270);
      g.fillStyle = "#f4f5f8";
      g.fillRect((performance.now() / 10) % 480, 75, 120, 120);
    }, 33);
    const peer = new RTCPeerConnection();
    const [track] = canvas.captureStream(30).getVideoTracks();
    if (!track) throw new Error("the canvas gave no video track");
    peer.addTrack(track);
    this.peers.set(session, { peer, timer });
    await peer.setRemoteDescription({ type: "offer", sdp: offer });
    await peer.setLocalDescription(await peer.createAnswer());
    await new Promise<void>((done) => {
      if (peer.iceGatheringState === "complete") return done();
      peer.addEventListener("icegatheringstatechange", () => peer.iceGatheringState === "complete" && done());
    });
    return peer.localDescription?.sdp ?? "";
  },
  end(session: number) {
    const ended = this.peers.get(session);
    if (!ended) throw { kind: "refused", error: { code: "not_found", error: `no WHEP session ${session}` } };
    window.clearInterval(ended.timer);
    ended.peer.close();
    this.peers.delete(session);
  },
};

const handlers: Record<string, (args: Args) => unknown> = {
  status: () => state.status,
  theme: () => state.theme,
  streams: () => ({ streams: STREAMS }),
  stream_snapshot: ({ name }) => {
    if (!STREAMS.some((s) => s.name === name)) throw { kind: "refused", error: { code: "not_found", error: `no stream ${String(name)} is attached` } };
    return monitorFrame();
  },
  watch_stream: async ({ name, offer }) => {
    if (!STREAMS.some((s) => s.name === name)) throw { kind: "refused", error: { code: "not_found", error: `no stream ${String(name)} is attached` } };
    const session = ++watching.next;
    const answer = await watching.answer(session, offer as string);
    return { stream: name, session, answer };
  },
  stop_watching: ({ session }) => {
    watching.end(session as number);
    return null;
  },
  apply_theme: ({ theme }) => {
    if (state.status.link.connection.state !== "connected") {
      throw { kind: "glassdeckd", error: { kind: "refused", error: { code: "unavailable", error: "the keyboard isn't connected" } } };
    }
    state.theme = structuredClone(theme as Theme);
    glassdeckd({ state: "message", event: { type: "theme", theme: state.theme } });
    return { stream: null };
  },
  list_widgets: () => state.widgets,
  create_widget: ({ widget: created }) => {
    const { title, id, sizing } = created as NewWidget;
    const summary: WidgetSummary = { id: `widget.${id}`, title, version: "1.0.0", sizing, has_options: false, runs_on_host: false, preview: null };
    state.widgets.push(summary);
    return summary;
  },
  duplicate_package: ({ id }) => `${id as string}-copy`,
  rename_package: ({ id, title }) => {
    const all: { id: string; title: string }[] = [...state.widgets, ...state.backgrounds];
    const found = all.find((p) => p.id === id);
    if (!found) throw notFound(id as string);
    found.title = title as string;
    return null;
  },
  delete_package: ({ id }) => {
    state.widgets = state.widgets.filter((w) => w.id !== id);
    state.backgrounds = state.backgrounds.filter((b) => b.id !== id);
    return null;
  },
  reveal_package: () => null,
  export_package: () => null,
  import_widget: () => null,
  pick_video: () => null,
  refresh_message_types: ({ messages }) => messageProblems(messages as string),
  pick_image: () => "C:\\Pictures\\mountain-dawn.jpg",
  preview_image: async () => {
    await new Promise((done) => window.setTimeout(done, PICTURE_LOAD_MS));
    return pictureFrame();
  },
  read_image: ({ id }) => imageBackground(id as string, "mountain-dawn.jpg"),
  replace_image: ({ id, source }) => imageBackground(id as string, (source as string).split(/[\\/]/).pop() ?? ""),
  list_backgrounds: () => state.backgrounds,
  create_background: ({ background: created }) => {
    const { title, id, content } = created as NewBackground;
    const summary: BackgroundSummary = { id: `background.${id}`, title, version: "1.0.0", kind: content.kind, preview: null };
    state.backgrounds.push(summary);
    return summary;
  },
  canvas_template: () => CANVAS_FILES,
  read_canvas: ({ id }) => ({ background: background(id as string), files: CANVAS_FILES }),
  save_canvas: ({ id }) => background(id as string),
  check_canvas: () => ({ shader_error: null, problems: [], checked: 3 }),
  preview_canvas: ({ files, run }) => startCanvas(JSON.stringify(files).length.toString(), run as number),
  canvas_options: () => undefined,
  play_canvas: () => undefined,
  restart_canvas: () => undefined,
  stop_canvas: () => stopCanvas(),
  read_video: ({ id }) => ({ background: background(id as string), record: { from: "rain-window.mp4", cut: { crop: 0.5, start: 0, end: 42 } }, video: null, conversion: null }),
  video_facts: () => ({ codec: "HEVC", width: 3840, height: 1600, fps: 30, length: 42, fits_panel: false }),
  video_frame: () => canvasFrame("video", 0),
  set_video_preview: () => canvasFrame("video", 0),
  replace_video: () => null,
  panel_layout: () => PANEL_LAYOUT,
  preview_layout: ({ widgets, run }) => {
    showLayout(widgets as PlacedSource[], run as number);
    return null;
  },
  stop_layout: () => undefined,
  read_widget: ({ id }) => ({ widget: widget(id as string), files: widgetFiles(id as string) }),
  save_widget: ({ id }) => widget(id as string),
  check_widget: () => ({ problems: [], checked: 2 }),
  widget_options: ({ source, values }) => optionsPanel(source as string, values as Args),
  preview_widget: ({ id, files, size, run }) => {
    showWidget(id as string, files as SourceFile[], size as Dims, run as number);
    return null;
  },
  preview_resize: () => undefined,
  preview_options: () => undefined,
  stop_preview: () => undefined,
  request_still: ({ id }) => {
    window.setTimeout(() => drawStill(id as string), 50);
    return undefined;
  },
  built_in_still: () => null,
  set_preview_time: ({ id }) => {
    window.setTimeout(() => drawStill(id as string), 50);
    return background(id as string);
  },
  keyboard: () => {
    if (keyboard.running) throw { kind: "busy" };
    return mockKeyboard();
  },
  default_backups_folder: () => BACKUPS_FOLDER,
  pick_backups_folder: () => null,
  keyboard_backups: () => keyboard.backups,
  back_up_keyboard: ({ folder }) => {
    if (keyboard.running) throw { kind: "busy" };
    runBackup(folder as string);
    return null;
  },
  install_glassdeck: () => {
    throw { kind: "unavailable", reason: "Installing isn't ready yet: the keyboard's launcher doesn't know how Flux starts its bridge" };
  },
  restore_flux_software: () => {
    throw { kind: "unavailable", reason: `The backup of ${MOCK_SERIAL} checked out, but restoring isn't ready yet` };
  },
  pick_backup: ({ folder }) => (folder ? "F:\\keyboard-backups\\" + MOCK_SERIAL : "F:\\backups\\copy.zip"),
  import_backup: ({ folder }) => {
    const imported = mockBackup(folder as string, 1_795_800_000);
    keyboard.backups = [...keyboard.backups.filter((b) => b.file !== imported.file), imported];
    return imported;
  },
  export_backup: ({ file }) => `D:\\${String(file).split("\\").pop()}`,
  reveal_backup: () => null,
  lsp_start: () => ({ session: state.nextLspSession++, root: "file:///mock/package", settings: {} }),
  lsp_send: ({ session, message }) => {
    fakeLanguageServer(session as number, JSON.parse(message as string) as LspRequest);
    return null;
  },
  lsp_stop: () => null,
};

type LspRequest = { id?: number; method: string; params?: Args };

/** Enough of a language server for the editor's tests: a type error for a string given to a number, script completions, a hover. */
function fakeLanguageServer(session: number, request: LspRequest) {
  const reply = (message: object) => window.setTimeout(() => void emit("lsp-message", { session, message: JSON.stringify({ jsonrpc: "2.0", ...message }) }));
  const params = request.params ?? {};
  switch (request.method) {
    case "initialize":
      return reply({ id: request.id, result: { capabilities: {} } });
    case "textDocument/didOpen":
    case "textDocument/didChange": {
      const document = (params.textDocument ?? {}) as { uri: string; text?: string };
      const text = document.text ?? ((params.contentChanges as { text: string }[] | undefined)?.at(-1)?.text ?? "");
      const diagnostics = text.split("\n").flatMap((line, i) => {
        const at = line.indexOf('"three"');
        return at < 0 ? [] : [{ range: { start: { line: i, character: at }, end: { line: i, character: at + 7 } }, severity: 1, message: "Type 'string' could not be converted into 'number'" }];
      });
      return reply({ method: "textDocument/publishDiagnostics", params: { uri: document.uri, diagnostics } });
    }
    case "textDocument/completion":
      return reply({
        id: request.id,
        result: [
          { label: "on_draw", kind: 2, detail: "(self, (ui: Ui) -> ()) -> ()" },
          { label: "every", kind: 2, detail: "(self, seconds: number, () -> ()) -> ()" },
        ],
      });
    case "textDocument/hover":
      return reply({ id: request.id, result: { contents: { kind: "markdown", value: "```luau\nwidget: Widget\n```" } } });
    default:
      if (request.id !== undefined) reply({ id: request.id, result: null });
  }
}

/** A still the way the real worker draws one: a widget's first tile, or a canvas at time 0. Videos have none until converted. */
function drawStill(id: string) {
  const found = state.widgets.find((w) => w.id === id);
  const kind = id === BUILT_IN_BACKGROUND ? "canvas" : state.backgrounds.find((b) => b.id === id)?.kind;
  const path = found
    ? `data:image/png;base64,${widgetTile(found.title, found.sizing.size.w, found.sizing.size.h, 0)}`
    : kind === "canvas"
      ? `data:image/jpeg;base64,${canvasFrame(id, 0)}`
      : kind === "image"
        ? `data:image/jpeg;base64,${pictureFrame()}`
        : null;
  const update = path ? { state: "ready", id, still: { path, stamp: Date.now() } } : { state: "failed", id, reason: "a video's still comes from converting it" };
  void emit("still-update", update);
}

/** What the real parser says about a message kind that isn't one: enough for the editor's tests. */
function messageProblems(source: string) {
  return source.split("\n").flatMap((line, i) => {
    const kind = /=\s*(\w+)</.exec(line)?.[1];
    if (!kind || ["KeyboardState", "KeyboardEvent", "HostEvent"].includes(kind)) return [];
    return [{ line: i + 1, column: line.indexOf(kind) + 1, message: `${kind} isn't a kind of message: use KeyboardState, KeyboardEvent or HostEvent` }];
  });
}

/** An image background as its page reads it; `from`: the picture's file name. */
function imageBackground(id: string, from: string) {
  return { background: background(id), record: { from }, path: `C:\\mock\\${id}\\image.jpg`, width: 1920, height: 1080 };
}

/** Lets a test put the fake keyboard in another state, as glassdeckd would report it. */
function setKeyboard(connection: KeyboardLink) {
  state.status = { ...state.status, link: { ...state.status.link, connection } };
  glassdeckd({ state: "message", event: { type: "link", connection } });
}

declare global {
  interface Window {
    glassdeckMock?: { setKeyboard: typeof setKeyboard; staleFrame: string; watching: () => number; dragFiles: typeof dragFiles };
  }
}

/** Files dragged over the window, as Tauri reports them: `enter`, then `drop` or `leave`. */
function dragFiles(type: "enter" | "drop" | "leave", paths: string[] = []) {
  void emit(`tauri://drag-${type}`, { paths, position: { x: 400, y: 300 } });
}

export function installMockBackend() {
  mockWindows("main");
  mockConvertFileSrc("windows");
  mockIPC(
    (cmd, payload) => {
      const handler = handlers[cmd];
      if (!handler) throw new Error(`mock backend: no handler for ${cmd}`);
      return handler((payload ?? {}) as Args);
    },
    { shouldMockEvents: true },
  );
  window.glassdeckMock = { setKeyboard, staleFrame: STALE_FRAME, watching: () => watching.peers.size, dragFiles };
}
