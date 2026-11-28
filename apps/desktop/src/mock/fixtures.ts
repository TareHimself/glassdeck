import { BUILT_IN_BACKGROUND, type BackgroundSummary, type PanelLayout, type PanelRect, type SourceFile, type Status, type StreamEntry, type Theme, type WidgetSummary } from "../bindings";

export const WIDGETS: WidgetSummary[] = [
  { id: "widget.local.user.clock", title: "Clock", version: "1.0.0", sizing: { kind: "range", size: { w: 320, h: 160 }, min: { w: 160, h: 80 }, max: { w: 640, h: 248 } }, has_options: true, runs_on_host: false, preview: null },
  { id: "widget.local.user.now-playing", title: "Now playing", version: "1.0.0", sizing: { kind: "any", size: { w: 400, h: 160 } }, has_options: false, runs_on_host: true, preview: null },
  { id: "widget.local.user.pomodoro", title: "Pomodoro", version: "1.0.0", sizing: { kind: "fixed", size: { w: 160, h: 160 } }, has_options: false, runs_on_host: false, preview: null },
];

export const BACKGROUNDS: BackgroundSummary[] = [
  { id: "background.local.user.aurora", title: "Aurora", version: "1.0.0", kind: "canvas", preview: null },
  { id: "background.local.user.rain", title: "Rain on glass", version: "1.0.0", kind: "video", preview: null },
  { id: "background.local.user.mountain-dawn", title: "Mountain dawn", version: "1.0.0", kind: "image", preview: null },
];

export const THEME: Theme = {
  format: 1,
  background: { package: BUILT_IN_BACKGROUND, options: {} },
  stream: null,
  widgets: [{ package: "widget.local.user.clock", x: 0, y: 16, w: 320, h: 160, options: {} }],
};

export const STATUS: Status = {
  link: { keyboard: "10.0.0.186:7878", connection: { state: "connected" } },
  stream: { state: "idle", playing: null, worker: null, streaming: false, last_ended: null },
};

export const STREAMS: StreamEntry[] = [{ name: "monitor-1", mode: "live", from: "built in", shown: false }];

export const CLOCK_KEYBOARD = `--[[ Draws the time ]]
local seconds: boolean = false

bridge.on.options(function(options: Options)
    seconds = options.seconds
end)

script.on_draw(function(ui: Ui)
    local label = \`{if seconds then "%H:%M:%S" else "%H:%M"}\`
    ui:text(os.date(label), { size = ui.height * 0.5 })
end)
`;

export const CLOCK_OPTIONS =`return function(o)
  o.toggle("seconds", { label = "Show seconds", default = true })
end
`;

export const CANVAS_SLANG = `import glassdeck;

/* Colours the options pick */
[Inputs]
struct Inputs {
    half3 night; half3 rose;
    float2 roseAt;
}

[Canvas]
half4 gradient(Pixel p, Inputs i) {
    // Fade from night at the top to rose
    half3 c = lerp(i.night, i.rose, half(p.uv.y) * 0.75h);
    return half4(c, 1);
}
`;

export const CANVAS_FILES: SourceFile[] = [
  { name: "canvas.slang", text: CANVAS_SLANG },
  { name: "keyboard.luau", text: "" },
  { name: "options.luau", text: "" },
  { name: "host.luau", text: "" },
  { name: "bridge.luau", text: "" },
];

/** The ANSI keys, row by row, in the same units glassdeck-layout uses (mm, 17 mm keys, 2 mm gaps). */
function ansiKeys(): PanelRect[] {
  const PX = 1920 / 309.31;
  const U = 17;
  const GAP = 2;
  const gapsBelow = [6.75, 2, 2, 2, 2, 3.07];
  const rows = [
    Array<number>(16).fill(U),
    [U, U, U, U, U, U, U, U, U, U, U, U, U, 36, U],
    [26.53, U, U, U, U, U, U, U, U, U, U, U, U, 26.53, U],
    [31.29, U, U, U, U, U, U, U, U, U, U, U, 40.82, U],
    [40.82, U, U, U, U, U, U, U, U, U, U, 31.29, U, U],
    [21.77, U, U, U, 107.49, U, U, U, U, U, U],
  ];
  const tops: number[] = [];
  let bottom = 1080 / PX;
  for (let r = rows.length - 1; r >= 0; r--) {
    bottom -= gapsBelow[r] ?? 0;
    tops[r] = bottom - U;
    bottom = tops[r] ?? 0;
  }
  return rows.flatMap((row, r) => {
    const span = row.reduce((a, b) => a + b, 0) + (row.length - 1) * GAP;
    let left = (309.31 - span) / 2;
    return row.map((w) => {
      const rect = { x: left * PX, y: (tops[r] ?? 0) * PX, w: w * PX, h: U * PX };
      left += w + GAP;
      return rect;
    });
  });
}

/** The default keymap's base-layer labels, as `glassdeck_layout::keymap` gives them. */
const KEY_LABELS = ["Esc", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "PrtSc", "Ins", "Delete", "`", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "=", "Backspace", "Home", "Tab", "Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P", "[", "]", "\\", "End", "Caps", "A", "S", "D", "F", "G", "H", "J", "K", "L", ";", "'", "Enter", "PgUp", "Shift", "Z", "X", "C", "V", "B", "N", "M", ",", ".", "/", "Shift", "↑", "PgDn", "Ctrl", "Fn", "Win", "Alt", "Space", "Alt", "Fn", "Ctrl", "←", "↓", "→"];

export const PANEL_LAYOUT: PanelLayout = { keys: ansiKeys(), labels: KEY_LABELS, widget_area: { x: 8.1, y: 5, w: 1903.8, h: 250 } };
