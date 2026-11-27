import { emit } from "@tauri-apps/api/event";
import { mockConvertFileSrc, mockIPC } from "@tauri-apps/api/mocks";
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
import { BACKGROUNDS, CANVAS_FILES, CLOCK_OPTIONS, PANEL_LAYOUT, STATUS, STREAMS, THEME, WIDGETS } from "./fixtures";
import { canvasFrame, widgetTile } from "./frames";

/** The app's native side, faked in memory for running the frontend in a plain browser. */
const state = {
  widgets: structuredClone(WIDGETS),
  backgrounds: structuredClone(BACKGROUNDS),
  theme: structuredClone(THEME),
  status: structuredClone(STATUS),
  canvas: null as { id: string; started: number; timer: number } | null,
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

function startCanvas(id: string) {
  stopCanvas();
  const started = performance.now();
  const timer = window.setInterval(() => {
    const time = (performance.now() - started) / 1000;
    void emit("canvas-update", { state: "frame", jpeg: canvasFrame(id, time), time });
  }, 100);
  state.canvas = { id, started, timer };
}

function stopCanvas() {
  if (state.canvas) window.clearInterval(state.canvas.timer);
  state.canvas = null;
}

/** Each placed widget's first tile, the way the layout preview sends them. */
function showLayout(widgets: PlacedSource[]) {
  widgets.forEach((placed, index) => {
    const found = state.widgets.find((w) => w.id === placed.package);
    const update = found
      ? { state: "tile", index, width: placed.size.w, height: placed.size.h, png: widgetTile(found.title, placed.size.w, placed.size.h, 0) }
      : { state: "stopped", index, reason: `${placed.package} isn't in the library` };
    void emit("layout-update", update);
  });
}

function showWidget(id: string, size: Dims) {
  void emit("preview-update", { state: "frame", width: size.w, height: size.h, png: widgetTile(widget(id).title, size.w, size.h, 0) });
}

function widgetFiles(id: string): SourceFile[] {
  const options = id.endsWith(".clock") ? CLOCK_OPTIONS : "";
  return [
    { name: "keyboard.luau", text: "" },
    { name: "options.luau", text: options },
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
    const summary: WidgetSummary = { id: `widget.${id}`, title, version: "1.0.0", sizing, has_options: false, runs_on_host: false };
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
    const summary: BackgroundSummary = { id: `background.${id}`, title, version: "1.0.0", kind: content.kind };
    state.backgrounds.push(summary);
    return summary;
  },
  canvas_template: () => CANVAS_FILES,
  read_canvas: ({ id }) => ({ background: background(id as string), files: CANVAS_FILES }),
  save_canvas: ({ id }) => background(id as string),
  check_canvas: () => ({ shader_error: null, problems: [], checked: 3 }),
  preview_canvas: ({ files }) => startCanvas(JSON.stringify(files).length.toString()),
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
  preview_layout: ({ widgets }) => {
    showLayout(widgets as PlacedSource[]);
    return null;
  },
  stop_layout: () => undefined,
  read_widget: ({ id }) => ({ widget: widget(id as string), files: widgetFiles(id as string) }),
  save_widget: ({ id }) => widget(id as string),
  check_widget: () => ({ problems: [], checked: 2 }),
  widget_options: ({ source, values }) => optionsPanel(source as string, values as Args),
  preview_widget: ({ id, size }) => {
    showWidget(id as string, size as Dims);
    return null;
  },
  preview_resize: () => undefined,
  preview_options: () => undefined,
  stop_preview: () => undefined,
};

/** Lets a test put the fake keyboard in another state, as glassdeckd would report it. */
function setKeyboard(connection: KeyboardLink) {
  state.status = { ...state.status, link: { ...state.status.link, connection } };
  glassdeckd({ state: "message", event: { type: "link", connection } });
}

declare global {
  interface Window {
    glassdeckMock?: { setKeyboard: typeof setKeyboard };
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
  window.glassdeckMock = { setKeyboard };
}
