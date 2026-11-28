import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { Link, useLocation } from "react-router";
import { BUILT_IN_BACKGROUND, commands, events, GRID, WIDGET_AREA, type BackgroundSummary, type PanelLayout, type PanelRect, type PreviewRun, type ThemeWidget, type WidgetSummary } from "../bindings";
import { Button } from "../components/Button";
import { BACK_TO_OVERVIEW, PageHeader } from "../components/PageHeader";
import { Select } from "../components/Select";
import { BackgroundThumb, StreamLayer } from "../components/Thumbs";
import { Problem } from "../components/Problem";
import { useApplyTheme } from "../applying";
import { setDraft, sameTheme, useSeededDraft } from "../draft";
import { nextPreviewRun } from "../previews";
import { describeError, refreshStreams, useGlassdeckd } from "../glassdeckd";
import { useKeyboard } from "../keyboard";
import { BUILT_IN, useBuiltInBackground } from "../library";
import { overlaps, type Box } from "../widgetArea";
import { SelectedWidget } from "./SelectedWidget";
import { addedWidget } from "./Widgets";
import styles from "./Overview.module.css";

const PANEL = { w: 1920, h: 1080 };

/** What the background card says when the theme names a background that isn't in the library. */
const MISSING_BACKGROUND = "Missing background";


/** A rectangle on the panel as percentages of the preview, which shows the whole panel. */
function placed(rect: PanelRect) {
  const percent = (value: number | null, of: number) => `${((value ?? 0) / of) * 100}%`;
  return { left: percent(rect.x, PANEL.w), top: percent(rect.y, PANEL.h), width: percent(rect.w, PANEL.w), height: percent(rect.h, PANEL.h) };
}

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
  const run = useRef<PreviewRun | null>(null);

  useEffect(() => {
    const listening = events.layoutUpdate.listen(({ payload }) => {
      if (payload.run !== run.current) return;
      const change = payload.change;
      setTiles((all) => {
        const next = [...all];
        next[change.index] = change.state === "tile" ? { png: change.png } : { stopped: change.reason };
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
    run.current = nextPreviewRun();
    void commands.previewLayout(
      widgets.map((w) => ({ package: w.package, size: { w: w.w, h: w.h }, values: w.options ?? {} })),
      run.current,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return tiles;
}

function useLibrary() {
  const [backgrounds, setBackgrounds] = useState<BackgroundSummary[]>([]);
  const [widgets, setWidgets] = useState<WidgetSummary[]>([]);
  const builtIn = useBuiltInBackground();
  useEffect(() => {
    void commands.listBackgrounds().then((r) => r.status === "ok" && setBackgrounds(r.data));
    void commands.listWidgets().then((r) => r.status === "ok" && setWidgets(r.data));
  }, []);
  return { backgrounds: [builtIn, ...backgrounds], widgets };
}

export function Overview() {
  const connection = useGlassdeckd((s) => s.connection);
  const needsSetup = useKeyboard((s) => s.keyboard?.software === "flux");
  const active = useGlassdeckd((s) => s.theme);
  const draft = useSeededDraft();
  const library = useLibrary();
  const [layout, setLayout] = useState<PanelLayout | null>(null);
  const streams = useGlassdeckd((s) => s.streams);
  const [showKeys, setShowKeys] = useState(true);
  const added = addedWidget(useLocation().state);
  const [selected, setSelected] = useState<number | null>(added);
  /** The widget being dragged, where it is now; the draft changes only when the drag ends. */
  const [ghost, setGhost] = useState<{ index: number; box: Box } | null>(null);
  const panel = useRef<HTMLDivElement>(null);
  const { apply: applyTheme, applying, problem } = useApplyTheme();

  useEffect(() => {
    void commands.panelLayout().then(setLayout);
  }, []);

  const streamStatus = connection.state === "connected" ? connection.status.stream : null;
  useEffect(() => {
    void refreshStreams();
  }, [streamStatus?.playing, streamStatus?.streaming]);

  /** The chosen stream, while it's attached: it overrides the background, on the keyboard and here. */
  const liveStream = draft?.stream && streams.some((s) => s.name === draft.stream) ? draft.stream : null;
  const pending = useMemo(() => !!draft && !!active && !sameTheme(draft, active), [draft, active]);
  const known = draft ? library.backgrounds.find((b) => b.id === draft.background.package) : undefined;
  const background = known ?? { ...BUILT_IN, id: draft?.background.package ?? BUILT_IN_BACKGROUND, title: MISSING_BACKGROUND };
  const tiles = useWidgetTiles(draft?.widgets ?? []);

  const apply = async () => {
    if (draft) await applyTheme(draft);
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
            {needsSetup ? (
              <>
                <Link className={styles.setUp} to="/keyboard">
                  Set up the keyboard to apply
                </Link>
                <Button variant="primary" disabled>
                  Apply to keyboard
                </Button>
              </>
            ) : (
              <Button variant={pending ? "primary" : "quiet"} className={pending ? undefined : styles.upToDate} disabled={!pending || applying} onClick={() => void apply()}>
                {pending ? (applying ? "Applying…" : "Apply to keyboard") : "Keyboard up to date"}
              </Button>
            )}
          </>
        }
      />

      {problem && <Problem>{problem}</Problem>}

      <div className={styles.columns}>
        <section aria-label="Keyboard preview" className={styles.preview}>
          <div className={styles.panel} ref={panel} onPointerDown={() => setSelected(null)}>
            <BackgroundThumb className={styles.layer} background={background} values={draft.background.options ?? {}} live={!liveStream} />
            {liveStream && <StreamLayer key={liveStream} className={styles.layer} name={liveStream} />}
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
                  {layout.labels[i]}
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
              <Link className={styles.linkButton} to="/choose/background">
                Change
              </Link>
              {!known || background.id === BUILT_IN_BACKGROUND ? (
                <Button disabled>Edit</Button>
              ) : (
                <Link className={styles.linkButton} to={`/backgrounds/${background.id}`} state={BACK_TO_OVERVIEW}>
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
              <Link className={styles.add} aria-label="Add widget" to="/choose/widget">
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
                  <path d="M8 3v10M3 8h10" />
                </svg>
              </Link>
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

    </div>
  );
}
