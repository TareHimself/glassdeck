import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { Link } from "react-router";
import { BUILT_IN_BACKGROUND, commands, events, GRID, WIDGET_AREA, type BackgroundSummary, type PanelLayout, type PanelRect, type StreamEntry, type ThemeWidget, type WidgetSummary } from "../bindings";
import { Button } from "../components/Button";
import { PageHeader } from "../components/PageHeader";
import { Select } from "../components/Select";
import { BackgroundThumb } from "../components/Thumbs";
import { Problem } from "../components/Problem";
import { seedDraft, setDraft, sameTheme, useDraft } from "../draft";
import { pngUrl, rememberThumbnail, useThumbnails } from "../previews";
import { describeError, keyboardProblem, useGlassdeckd } from "../glassdeckd";
import { describeLibraryError } from "../library";
import { NewBackgroundDialog } from "./NewBackgroundDialog";
import { NewWidgetDialog } from "./NewWidgetDialog";
import { BackgroundPicker, WidgetPicker } from "./Pickers";
import { SelectedWidget } from "./SelectedWidget";
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

/** What the background card says when the theme names a background that isn't in the library. */
const MISSING_BACKGROUND = "Missing background";

/** The built-in gradient, offered beside the library's backgrounds. */
const BUILT_IN: BackgroundSummary = { id: BUILT_IN_BACKGROUND, title: "Gradient", version: "", kind: "canvas" };

/** A rectangle on the panel as percentages of the preview, which shows the whole panel. */
function placed(rect: PanelRect) {
  const percent = (value: number | null, of: number) => `${((value ?? 0) / of) * 100}%`;
  return { left: percent(rect.x, PANEL.w), top: percent(rect.y, PANEL.h), width: percent(rect.w, PANEL.w), height: percent(rect.h, PANEL.h) };
}

type Box = { x: number; y: number; w: number; h: number };
type Size = { w: number; h: number };

/** How small and large a widget may be made, from its sizing in the library. */
function boundsOf(widget: WidgetSummary | undefined, placedSize: Size): { min: Size; max: Size } {
  const sizing = widget?.sizing;
  switch (sizing?.kind) {
    case "range":
      return { min: sizing.min, max: sizing.max };
    case "any":
      return { min: { w: GRID, h: GRID }, max: WIDGET_AREA };
    default:
      return { min: placedSize, max: placedSize };
  }
}

const snap = (value: number) => Math.round(value / GRID) * GRID;
const clamp = (value: number, low: number, high: number) => Math.max(low, Math.min(high, value));

/** Where a move or resize by (dx, dy) panel pixels lands: on the grid, inside the area and the widget's bounds. */
function dragged(start: Box, mode: "move" | "resize", dx: number, dy: number, bounds: { min: Size; max: Size }): Box {
  if (mode === "move") {
    return { ...start, x: clamp(snap(start.x + dx), 0, WIDGET_AREA.w - start.w), y: clamp(snap(start.y + dy), 0, WIDGET_AREA.h - start.h) };
  }
  return {
    ...start,
    w: clamp(snap(start.w + dx), bounds.min.w, Math.min(bounds.max.w, WIDGET_AREA.w - start.x)),
    h: clamp(snap(start.h + dy), bounds.min.h, Math.min(bounds.max.h, WIDGET_AREA.h - start.y)),
  };
}

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

type Tile = { png: string } | { stopped: string };

/** Every placed widget running at once; each one's latest tile, by its place in the layout. */
function useWidgetTiles(widgets: ThemeWidget[]): Tile[] {
  const [tiles, setTiles] = useState<Tile[]>([]);
  const placedWidgets = useRef(widgets);
  placedWidgets.current = widgets;
  const key = JSON.stringify(widgets.map((w) => [w.package, w.w, w.h, w.options]));

  useEffect(() => {
    const listening = events.layoutUpdate.listen(({ payload }) => {
      const id = placedWidgets.current[payload.index]?.package;
      if (payload.state === "tile" && id && !useThumbnails.getState()[id]) rememberThumbnail(id, pngUrl(payload.png));
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
  const [selected, setSelected] = useState<number | null>(null);
  /** The widget being dragged, where it is now; the draft changes only when the drag ends. */
  const [ghost, setGhost] = useState<{ index: number; box: Box } | null>(null);
  const panel = useRef<HTMLDivElement>(null);
  const [applying, setApplying] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

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

  const pending = useMemo(() => !!draft && !!active && !sameTheme(draft, active), [draft, active]);
  const known = draft ? library.backgrounds.find((b) => b.id === draft.background.package) : undefined;
  const background = known ?? { ...BUILT_IN, id: draft?.background.package ?? BUILT_IN_BACKGROUND, title: MISSING_BACKGROUND };
  const tiles = useWidgetTiles(draft?.widgets ?? []);

  const apply = async () => {
    if (!draft) return;
    setApplying(true);
    const result = await commands.applyTheme(draft);
    setApplying(false);
    if (result.status === "ok") return setProblem(null);
    const error = result.error;
    const keyboard = connection.state === "connected" ? keyboardProblem(connection.status.link.connection) : null;
    setProblem(error.kind === "glassdeckd" ? (keyboard ?? describeError(error.error)) : describeLibraryError(error.error));
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
  const placedWidgets = draft.widgets ?? [];
  const selectedWidget = selected === null ? undefined : placedWidgets[selected];

  const startDrag = (e: ReactPointerEvent, index: number, mode: "move" | "resize") => {
    e.stopPropagation();
    e.preventDefault();
    setSelected(index);
    const rect = panel.current?.getBoundingClientRect();
    const widget = placedWidgets[index];
    if (!rect || !widget) return;
    const scale = PANEL.w / rect.width;
    const start: Box = { x: widget.x, y: widget.y, w: widget.w, h: widget.h };
    const bounds = boundsOf(library.widgets.find((w) => w.id === widget.package), start);
    const others = placedWidgets.filter((_, i) => i !== index);
    let last = start;
    const move = (m: PointerEvent) => {
      const next = dragged(start, mode, (m.clientX - e.clientX) * scale, (m.clientY - e.clientY) * scale, bounds);
      if (others.some((o) => overlaps(next, o))) return;
      last = next;
      setGhost({ index, box: next });
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setGhost(null);
      if (last !== start) setDraft((d) => ({ ...d, widgets: (d.widgets ?? []).map((w, i) => (i === index ? { ...w, ...last } : w)) }));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  const area = layout?.widget_area;
  const knownStream = !draft.stream || streams.some((s) => s.name === draft.stream);
  /** The chosen stream is attached right now, so the keyboard shows it instead of the background. */
  const live = streams.some((s) => s.name === draft.stream);

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
          <div className={styles.panel} ref={panel} onPointerDown={() => setSelected(null)}>
            <BackgroundThumb className={styles.layer} background={background} values={draft.background.options ?? {}} live />
            {area && <div className={styles.area} style={placed(area)} />}
            {area &&
              placedWidgets.map((widget, i) => {
                const box = ghost?.index === i ? ghost.box : widget;
                const resizable = (() => {
                  const { min, max } = boundsOf(library.widgets.find((w) => w.id === widget.package), widget);
                  return min.w !== max.w || min.h !== max.h;
                })();
                return (
                  <div
                    key={i}
                    role="button"
                    tabIndex={0}
                    aria-label={`Select ${titleOf(widget.package)}`}
                    aria-pressed={selected === i}
                    className={styles.widget}
                    style={placed({ x: (area.x ?? 0) + box.x, y: (area.y ?? 0) + box.y, w: box.w, h: box.h })}
                    onPointerDown={(e) => startDrag(e, i, "move")}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") setSelected(i);
                    }}
                  >
                    {tileOf(tiles[i], titleOf(widget.package))}
                    {selected === i && (
                      <>
                        <span className={styles.geom}>
                          {box.x}, {box.y} {box.w}×{box.h}
                        </span>
                        {resizable && <span className={styles.handle} aria-hidden="true" onPointerDown={(e) => startDrag(e, i, "resize")} />}
                      </>
                    )}
                  </div>
                );
              })}
            {showKeys &&
              layout?.keys.map((key, i) => (
                <div key={i} className={styles.key} style={placed(key)}>
                  {KEY_LABELS[i]}
                </div>
              ))}
          </div>
        </section>

        <aside className={styles.aside}>
          <section className={styles.card}>
            <h2 className={styles.cardTitle}>Background</h2>
            <div className={styles.current}>
              <BackgroundThumb className={styles.thumb} background={background} />
              <div className={styles.currentText}>
                <div className={styles.currentTitle}>{background.title}</div>
              </div>
            </div>
            <div className={styles.twoButtons}>
              <Button onClick={() => setPicking(true)} aria-haspopup="dialog">
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
          </section>

          <section className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Stream</h2>
              <span className={styles.live} data-live={live || undefined} data-hidden={!draft.stream || undefined} aria-hidden={!draft.stream || undefined}>
                Live
              </span>
            </div>
            <Select
              label="Stream"
              mono
              value={draft.stream ?? ""}
              onChange={(stream) => setDraft((d) => ({ ...d, stream: stream || null }))}
              options={[
                { value: "", label: "None" },
                ...(knownStream ? [] : [{ value: draft.stream ?? "", label: draft.stream }]),
                ...streams.map((s) => ({ value: s.name, label: s.name })),
              ]}
            />
          </section>

          <section className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Widgets</h2>
              <button type="button" className={styles.add} aria-label="Add widget" aria-haspopup="dialog" onClick={() => setAdding(true)}>
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
                  <path d="M8 3v10M3 8h10" />
                </svg>
              </button>
            </div>
            <div className={styles.widgetList}>
              {placedWidgets.map((widget, i) => (
                <button key={i} type="button" className={styles.widgetRow} aria-pressed={selected === i} onClick={() => setSelected(i)}>
                  <span className={styles.widgetTitle}>{titleOf(widget.package)}</span>
                  <span className={styles.geometry}>
                    {widget.x}, {widget.y} {widget.w}×{widget.h}
                  </span>
                </button>
              ))}
            </div>
          </section>

          {selected !== null && selectedWidget && (
            <SelectedWidget
              key={`${selected}:${selectedWidget.package}`}
              widget={selectedWidget}
              summary={library.widgets.find((w) => w.id === selectedWidget.package)}
              onOptions={(options) => setDraft((d) => ({ ...d, widgets: (d.widgets ?? []).map((w, i) => (i === selected ? { ...w, options } : w)) }))}
            />
          )}
        </aside>
      </div>

      {picking && (
        <BackgroundPicker
          items={library.backgrounds}
          current={background.id}
          onPick={(choice) => {
            setPicking(false);
            if (choice.id !== background.id) setDraft((d) => ({ ...d, background: { package: choice.id, options: {} } }));
          }}
          onNew={() => {
            setPicking(false);
            setCreating(true);
          }}
          onClose={() => setPicking(false)}
        />
      )}
      {adding && (
        <WidgetPicker
          items={library.widgets}
          onPick={(widget) => {
            setAdding(false);
            const spot = freeSpot(widget.sizing.size, draft.widgets ?? []);
            if (!spot) return setProblem(`There's no room for ${widget.title} in the widget area`);
            setSelected(draft.widgets?.length ?? 0);
            setDraft((d) => ({ ...d, widgets: [...(d.widgets ?? []), { package: widget.id, ...spot, options: {} }] }));
          }}
          onNew={() => {
            setAdding(false);
            setCreatingWidget(true);
          }}
          onClose={() => setAdding(false)}
        />
      )}
      {creating && <NewBackgroundDialog onClose={() => setCreating(false)} />}
      {creatingWidget && <NewWidgetDialog onClose={() => setCreatingWidget(false)} />}
    </div>
  );
}
