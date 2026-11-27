import { emit } from "@tauri-apps/api/event";
import { mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
import { BUILT_IN_BACKGROUND } from "../bindings";
import type {
  BackgroundSummary,
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
import { canvasFrame, monitorFrame, staleFrame, widgetTile } from "./frames";

const STALE_FRAME = staleFrame();

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
      send(replaced, { state: "program", fragment: STALE_SHADER, width: 64, height: 36 });
      send(replaced, { state: "frame", time: 0, delta: 0, index: 0, inputs: [] });
    }, 150);
  }
  send(run, { state: "program", fragment: MOCK_SHADER, width: 480, height: 270 });
  const started = performance.now();
  let index = 0;
  let last = 0;
  const timer = window.setInterval(() => {
    const time = (performance.now() - started) / 1000;
    send(run, { state: "frame", time, delta: time - last, index: index++, inputs: [] });
    last = time;
  }, 33);
  state.canvas = { id, run, started, timer };
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

function showWidget(id: string, size: Dims, run: number) {
  void emit("preview-update", { run, change: { state: "frame", width: size.w, height: size.h, png: widgetTile(widget(id).title, size.w, size.h, 0) } });
}

function widgetFiles(id: string): SourceFile[] {
  const clock = id.endsWith(".clock");
  return [
    { name: "keyboard.luau", text: clock ? CLOCK_KEYBOARD : "" },
    { name: "options.luau", text: clock ? CLOCK_OPTIONS : "" },
    { name: "host.luau", text: "" },
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

const handlers: Record<string, (args: Args) => unknown> = {
  status: () => state.status,
  theme: () => state.theme,
  streams: () => ({ streams: STREAMS }),
  stream_snapshot: ({ name }) => {
    if (!STREAMS.some((s) => s.name === name)) throw { kind: "refused", error: { code: "not_found", error: `no stream ${String(name)} is attached` } };
    return monitorFrame();
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
  preview_widget: ({ id, size, run }) => {
    showWidget(id as string, size as Dims, run as number);
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
  lsp_start: () => ({ session: state.nextLspSession++, root: "file:///mock/package", settings: {} }),
  lsp_send: ({ session, message }) => {
    fakeLanguageServer(session as number, JSON.parse(message as string) as LspRequest);
    return null;
  },
  lsp_stop: () => null,
};

type LspRequest = { id?: number; method: string; params?: Args };

/** Enough of a language server for the editor's tests: a type error for a string given to a number, widget completions, a hover. */
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
  const canvas = id === BUILT_IN_BACKGROUND || state.backgrounds.some((b) => b.id === id && b.kind === "canvas");
  const path = found
    ? `data:image/png;base64,${widgetTile(found.title, found.sizing.size.w, found.sizing.size.h, 0)}`
    : canvas
      ? `data:image/jpeg;base64,${canvasFrame(id, 0)}`
      : null;
  const update = path ? { state: "ready", id, still: { path, stamp: Date.now() } } : { state: "failed", id, reason: "a video's still comes from converting it" };
  void emit("still-update", update);
}

/** Lets a test put the fake keyboard in another state, as glassdeckd would report it. */
function setKeyboard(connection: KeyboardLink) {
  state.status = { ...state.status, link: { ...state.status.link, connection } };
  glassdeckd({ state: "message", event: { type: "link", connection } });
}

declare global {
  interface Window {
    glassdeckMock?: { setKeyboard: typeof setKeyboard; staleFrame: string };
  }
}

export function installMockBackend() {
  mockConvertFileSrc("windows");
  mockIPC(
    (cmd, payload) => {
      const handler = handlers[cmd];
      if (!handler) throw new Error(`mock backend: no handler for ${cmd}`);
      return handler((payload ?? {}) as Args);
    },
    { shouldMockEvents: true },
  );
  window.glassdeckMock = { setKeyboard, staleFrame: STALE_FRAME };
}
