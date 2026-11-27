import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import { BUILT_IN_BACKGROUND, commands, events, GRID, WIDGET_AREA, type BackgroundKind, type BackgroundSummary, type PanelLayout, type PanelRect, type StreamEntry, type ThemeWidget, type Value, type WidgetSummary } from "../bindings";
import { Button } from "../components/Button";
import { Menu, MenuDivider, MenuItem } from "../components/Menu";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { seedDraft, setDraft, sameTheme, useDraft } from "../draft";
import { describeError, useGlassdeckd } from "../glassdeckd";
import { describeLibraryError } from "../library";
import { CanvasIcon, VideoIcon } from "./backgroundKinds";
import { NewBackgroundDialog } from "./NewBackgroundDialog";
import { NewWidgetDialog } from "./NewWidgetDialog";
import styles from "./Overview.module.css";

const PANEL = { w: 1920, h: 1080 };

/** The default keymap's base layer, in the layout's key order (row by row, left to right). */
const KEY_LABELS = [
  ["Esc", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "PrtSc", "Ins", "Del"],
  ["`", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "=", "Backspace", "Home"],
  ["Tab", "Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P", "[", "]", "\\", "PgUp"],
  ["Caps", "A", "S", "D", "F", "G", "H", "J", "K", "L", ";", "'", "Enter", "PgDn"],
  ["Shift", "Z", "X", "C", "V", "B", "N", "M", ",", ".", "/", "Shift", "↑", "End"],
  ["Ctrl", "Win", "Alt", "Fn", "", "Alt", "Fn", "Ctrl", "←", "↓", "→"],
].flat();

/** The built-in gradient, offered beside the library's backgrounds. */
const BUILT_IN: BackgroundSummary = { id: BUILT_IN_BACKGROUND, title: "Gradient", version: "", kind: "canvas" };

/** A rectangle on the panel as percentages of the preview, which shows the whole panel. */
function placed(rect: PanelRect) {
  const percent = (value: number | null, of: number) => `${((value ?? 0) / of) * 100}%`;
  return { left: percent(rect.x, PANEL.w), top: percent(rect.y, PANEL.h), width: percent(rect.w, PANEL.w), height: percent(rect.h, PANEL.h) };
}

type Box = { x: number; y: number; w: number; h: number };

function overlaps(a: Box, b: Box): boolean {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

/** The first grid spot, top row first, where a widget of `size` fits without overlapping `taken`. */
function freeSpot(size: { w: number; h: number }, taken: Box[]): Box | null {
  for (let y = 0; y + size.h <= WIDGET_AREA.h; y += GRID) {
    for (let x = 0; x + size.w <= WIDGET_AREA.w; x += GRID) {
      const box = { x, y, ...size };
      if (!taken.some((t) => overlaps(box, t))) return box;
    }
  }
  return null;
}

function tileOf(tile: Tile | undefined, title: string) {
  if (tile && "png" in tile) return <img className={styles.tile} alt={title} src={`data:image/png;base64,${tile.png}`} />;
  return <span title={tile?.stopped}>{title}</span>;
}

function KindIcon({ kind }: { kind: BackgroundKind }) {
  return kind === "canvas" ? <CanvasIcon /> : <VideoIcon />;
}

type Options = { [key: string]: Value };

/** The background layer: a canvas drawn live by the canvas preview, or a video's converted file. */
function useBackgroundLayer(background: BackgroundSummary, options: Options): { kind: "canvas"; jpeg: string | null } | { kind: "video"; src: string | null } {
  const [jpeg, setJpeg] = useState<string | null>(null);
  const [src, setSrc] = useState<string | null>(null);
  const optionsKey = JSON.stringify(options);

  useEffect(() => {
    setJpeg(null);
    setSrc(null);
    if (background.kind === "video") {
      void commands.readVideo(background.id).then((r) => {
        if (r.status === "ok" && r.data.video) setSrc(convertFileSrc(r.data.video.path));
      });
      return;
    }
    const listening = events.canvasUpdate.listen(({ payload }) => {
      if (payload.state === "frame") setJpeg(payload.jpeg);
    });
    const files = background.id === BUILT_IN_BACKGROUND ? commands.canvasTemplate() : commands.readCanvas(background.id).then((r) => (r.status === "ok" ? r.data.files : []));
    void files.then((list) => {
      if (list.length > 0) void commands.previewCanvas(list, options);
    });
    return () => {
      void listening.then((unlisten) => unlisten());
      void commands.stopCanvas();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [background.id, background.kind, optionsKey]);

  return background.kind === "video" ? { kind: "video", src } : { kind: "canvas", jpeg };
}

type Tile = { png: string } | { stopped: string };

/** Every placed widget running at once; each one's latest tile, by its place in the layout. */
function useWidgetTiles(widgets: ThemeWidget[]): Tile[] {
  const [tiles, setTiles] = useState<Tile[]>([]);
  const key = JSON.stringify(widgets.map((w) => [w.package, w.w, w.h, w.options]));

  useEffect(() => {
    const listening = events.layoutUpdate.listen(({ payload }) => {
      setTiles((all) => {
        const next = [...all];
        next[payload.index] = payload.state === "tile" ? { png: payload.png } : { stopped: payload.reason };
        return next;
      });
    });
    return () => {
      void listening.then((unlisten) => unlisten());
      void commands.stopLayout();
    };
  }, []);

  useEffect(() => {
    setTiles((all) => all.slice(0, widgets.length));
    void commands.previewLayout(widgets.map((w) => ({ package: w.package, size: { w: w.w, h: w.h }, values: w.options ?? {} })));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return tiles;
}

function useLibrary() {
  const [backgrounds, setBackgrounds] = useState<BackgroundSummary[]>([]);
  const [widgets, setWidgets] = useState<WidgetSummary[]>([]);
  useEffect(() => {
    void commands.listBackgrounds().then((r) => r.status === "ok" && setBackgrounds(r.data));
    void commands.listWidgets().then((r) => r.status === "ok" && setWidgets(r.data));
  }, []);
  return { backgrounds: [BUILT_IN, ...backgrounds], widgets };
}

export function Overview() {
  const connection = useGlassdeckd((s) => s.connection);
  const active = useGlassdeckd((s) => s.theme);
  const draft = useDraft((s) => s.draft);
  const library = useLibrary();
  const [layout, setLayout] = useState<PanelLayout | null>(null);
  const [streams, setStreams] = useState<StreamEntry[]>([]);
  const [showKeys, setShowKeys] = useState(true);
  const [picking, setPicking] = useState(false);
  const [creating, setCreating] = useState(false);
  const [creatingWidget, setCreatingWidget] = useState(false);
  const [adding, setAdding] = useState(false);
  const [applying, setApplying] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const backgroundCard = useRef<HTMLElement>(null);

  useEffect(() => {
    if (active) seedDraft(active);
  }, [active]);

  useEffect(() => {
    void commands.panelLayout().then(setLayout);
  }, []);

  const streamStatus = connection.state === "connected" ? connection.status.stream : null;
  useEffect(() => {
    void commands.streams().then((r) => r.status === "ok" && setStreams(r.data.streams));
  }, [streamStatus?.playing, streamStatus?.streaming]);

  useEffect(() => {
    if (!picking) return;
    const close = (e: PointerEvent) => {
      if (!backgroundCard.current?.contains(e.target as Node)) setPicking(false);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [picking]);

  const pending = useMemo(() => !!draft && !!active && !sameTheme(draft, active), [draft, active]);
  const known = draft ? library.backgrounds.find((b) => b.id === draft.background.package) : undefined;
  const background = known ?? { ...BUILT_IN, id: draft?.background.package ?? BUILT_IN_BACKGROUND, title: draft?.background.package ?? "" };
  const backgroundLayer = useBackgroundLayer(background, draft?.background.options ?? {});
  const tiles = useWidgetTiles(draft?.widgets ?? []);

  const apply = async () => {
    if (!draft) return;
    setApplying(true);
    const result = await commands.applyTheme(draft);
    setApplying(false);
    if (result.status === "ok") return setProblem(null);
    const error = result.error;
    setProblem(error.kind === "glassdeckd" ? describeError(error.error) : describeLibraryError(error.error));
  };

  if (connection.state === "lost") {
    return (
      <div className={styles.page}>
        <PageHeader title="Overview" />
        <Problem>{describeError(connection.error)}</Problem>
      </div>
    );
  }
  if (!draft) return <PageHeader title="Overview" />;

  const titleOf = (id: string) => library.widgets.find((w) => w.id === id)?.title ?? id;
  const area = layout?.widget_area;
  const knownStream = !draft.stream || streams.some((s) => s.name === draft.stream);
  const live = streams.some((s) => s.name === draft.stream && s.shown);

  return (
    <div className={styles.page}>
      <PageHeader
        title="Overview"
        actions={
          <>
            <Button onClick={() => setShowKeys(!showKeys)}>{showKeys ? "Hide keys" : "Show keys"}</Button>
            <Button variant={pending ? "primary" : "quiet"} className={pending ? undefined : styles.upToDate} disabled={!pending || applying} onClick={() => void apply()}>
              {pending ? (applying ? "Applying…" : "Apply to keyboard") : "Keyboard up to date"}
            </Button>
          </>
        }
      />

      {problem && <Problem>{problem}</Problem>}

      <div className={styles.columns}>
        <section aria-label="Keyboard preview" className={styles.preview}>
          <div className={styles.panel}>
            {backgroundLayer.kind === "canvas" && backgroundLayer.jpeg && <img className={styles.layer} alt="" src={`data:image/jpeg;base64,${backgroundLayer.jpeg}`} />}
            {backgroundLayer.kind === "video" && backgroundLayer.src && <video className={styles.layer} src={backgroundLayer.src} autoPlay loop muted playsInline />}
            {area && <div className={styles.area} style={placed(area)} />}
            {area &&
              (draft.widgets ?? []).map((widget, i) => (
                <div
                  key={i}
                  className={styles.widget}
                  style={placed({ x: (area.x ?? 0) + widget.x, y: (area.y ?? 0) + widget.y, w: widget.w, h: widget.h })}
                >
                  {tileOf(tiles[i], titleOf(widget.package))}
                </div>
              ))}
            {showKeys &&
              layout?.keys.map((key, i) => (
                <div key={i} className={styles.key} style={placed(key)}>
                  {KEY_LABELS[i]}
                </div>
              ))}
          </div>
        </section>

        <aside className={styles.aside}>
          <section className={styles.card} ref={backgroundCard}>
            <h2 className={styles.cardTitle}>Background</h2>
            <div className={styles.current}>
              <div className={styles.thumb} data-kind={background.kind} />
              <div className={styles.currentText}>
                <div className={styles.currentTitle}>{background.title}</div>
                <div className={styles.mono}>
                  {background.id} {background.version}
                </div>
              </div>
            </div>
            <div className={styles.twoButtons}>
              <Button onClick={() => setPicking(!picking)} aria-haspopup="menu" aria-expanded={picking}>
                Change
              </Button>
              {!known || background.id === BUILT_IN_BACKGROUND ? (
                <Button disabled>Edit</Button>
              ) : (
                <Link className={styles.linkButton} to={`/backgrounds/${background.id}`}>
                  Edit
                </Link>
              )}
            </div>
            {picking && (
              <Menu className={styles.picker}>
                {library.backgrounds.map((choice) => (
                  <MenuItem
                    key={choice.id}
                    role="menuitemradio"
                    aria-checked={choice.id === background.id}
                    className={styles.choice}
                    onClick={() => {
                      setDraft((d) => ({ ...d, background: { package: choice.id, options: {} } }));
                      setPicking(false);
                    }}
                  >
                    <span className={styles.choiceThumb} data-kind={choice.kind} />
                    <span className={styles.choiceTitle}>{choice.title}</span>
                    <KindIcon kind={choice.kind} />
                  </MenuItem>
                ))}
                <MenuDivider />
                <MenuItem
                  onClick={() => {
                    setPicking(false);
                    setCreating(true);
                  }}
                >
                  New background…
                </MenuItem>
              </Menu>
            )}
          </section>

          <section className={styles.card}>
            <h2 className={styles.cardTitle}>Stream</h2>
            <div className={styles.streamRow}>
              <select
                aria-label="Stream"
                className={styles.select}
                value={draft.stream ?? ""}
                onChange={(e) => setDraft((d) => ({ ...d, stream: e.target.value || null }))}
              >
                <option value="">None</option>
                {!knownStream && <option value={draft.stream ?? ""}>{draft.stream}</option>}
                {streams.map((s) => (
                  <option key={s.name} value={s.name}>
                    {s.name}
                  </option>
                ))}
              </select>
              <span className={styles.live} data-live={live || undefined}>
                Live
              </span>
            </div>
          </section>

          <section className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Widgets</h2>
              <button type="button" className={styles.add} aria-label="Add widget" aria-haspopup="menu" aria-expanded={adding} onClick={() => setAdding(!adding)}>
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
                  <path d="M8 3v10M3 8h10" />
                </svg>
              </button>
            </div>
            {adding && (
              <Menu className={styles.addMenu}>
                {library.widgets.map((widget) => (
                  <MenuItem
                    key={widget.id}
                    className={styles.addItem}
                    onClick={() => {
                      setAdding(false);
                      const spot = freeSpot(widget.sizing.size, draft.widgets ?? []);
                      if (!spot) return setProblem(`There's no room for ${widget.title} in the widget area`);
                      setDraft((d) => ({ ...d, widgets: [...(d.widgets ?? []), { package: widget.id, ...spot, options: {} }] }));
                    }}
                  >
                    <span>{widget.title}</span>
                    <span className={styles.geometry}>
                      {widget.sizing.size.w}×{widget.sizing.size.h}
                    </span>
                  </MenuItem>
                ))}
                <MenuDivider />
                <MenuItem
                  onClick={() => {
                    setAdding(false);
                    setCreatingWidget(true);
                  }}
                >
                  New widget…
                </MenuItem>
              </Menu>
            )}
            <div className={styles.widgetList}>
              {(draft.widgets ?? []).map((widget, i) => (
                <div key={i} className={styles.widgetRow}>
                  <span className={styles.widgetTitle}>{titleOf(widget.package)}</span>
                  <span className={styles.geometry}>
                    {widget.x}, {widget.y} {widget.w}×{widget.h}
                  </span>
                </div>
              ))}
            </div>
          </section>
        </aside>
      </div>

      {creating && <NewBackgroundDialog onClose={() => setCreating(false)} />}
      {creatingWidget && <NewWidgetDialog onClose={() => setCreatingWidget(false)} />}
    </div>
  );
}
