import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { commands, type LibraryError, type WidgetSummary } from "../bindings";
import { Button } from "../components/Button";
import { LibraryCard, type CardActions, type CardUse } from "../components/LibraryCard";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { SearchRow } from "../components/SearchRow";
import { Segments } from "../components/Segments";
import { Tag } from "../components/Tag";
import { WidgetThumb } from "../components/Thumbs";
import { placeWidget, useSeededDraft } from "../draft";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError, sizeLabel } from "../library";
import { matchesQuery } from "../search";
import { NewWidgetDialog } from "./NewWidgetDialog";
import styles from "./Widgets.module.css";

type Library = { state: "loading" } | { state: "ready"; widgets: WidgetSummary[] } | { state: "failed"; error: LibraryError };
type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };
type Filter = "all" | "in-use" | "host";

/** Router state the Overview reads to select a widget just added from the library. */
type AddedWidget = { selectWidget: number };

/** The index of the widget just added, from the Overview's router state; null when it came from elsewhere. */
export function addedWidget(state: unknown): number | null {
  if (typeof state === "object" && state !== null && "selectWidget" in state && typeof state.selectWidget === "number") return state.selectWidget;
  return null;
}

function HostIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="#8dbcff" strokeWidth="2">
      <rect x="2" y="3" width="18" height="12" rx="2" />
      <path d="M8 19h6M11 15v4" />
    </svg>
  );
}

function WidgetCard({ widget, inUse, use }: { widget: WidgetSummary; inUse: string | null; use: CardUse }) {
  return (
    <LibraryCard
      id={widget.id}
      title={widget.title}
      version={widget.version}
      inUse={inUse}
      use={use}
      preview={(live) => <WidgetThumb className={styles.preview} widget={widget} live={live} badge />}
      tags={
        <>
          <Tag>{widget.sizing.kind === "any" ? "Any size" : sizeLabel(widget.sizing)}</Tag>
          {widget.runs_on_host && (
            <Tag tone="host" role="img" aria-label="Runs on this device" title="Runs on this device">
              <HostIcon />
            </Tag>
          )}
        </>
      }
    />
  );
}

function useWidgets() {
  const [library, setLibrary] = useState<Library>({ state: "loading" });
  const reload = async () => {
    const result = await commands.listWidgets();
    setLibrary(result.status === "ok" ? { state: "ready", widgets: result.data } : { state: "failed", error: result.error });
  };
  useEffect(() => {
    void reload();
  }, []);
  return { library, reload };
}

/** The search row and what it lets through; `inUse` names the filter for the ones in `using`. */
function useShown(widgets: WidgetSummary[], using: Set<string>, inUse: string) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const fits = (w: WidgetSummary) => filter === "all" || (filter === "in-use" ? using.has(w.id) : w.runs_on_host);
  const shown = widgets.filter((w) => fits(w) && matchesQuery(query, w.title));
  const options: { value: Filter; name: string }[] = [
    { value: "all", name: "All" },
    { value: "in-use", name: inUse },
    { value: "host", name: "Runs on this device" },
  ];
  const search = (
    <SearchRow label="Search widgets" query={query} onQuery={setQuery}>
      <Segments compact label="Filter widgets" options={options} value={filter} onChange={setFilter} />
    </SearchRow>
  );
  const noMatch = widgets.length > 0 && shown.length === 0 && <div className={styles.noMatch}>No widgets match.</div>;
  return { shown, search, noMatch };
}

/** The widget library: each one to open in its editor, and new ones made or imported here. */
export function Widgets() {
  const navigate = useNavigate();
  const theme = useGlassdeckd((s) => s.theme);
  const { library, reload } = useWidgets();
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);
  const active = useMemo(() => new Set((theme?.widgets ?? []).map((w) => w.package)), [theme]);
  const { shown, search, noMatch } = useShown(library.state === "ready" ? library.widgets : [], active, "Active");

  const run = async <T,>(call: Promise<Answer<T>>): Promise<T | null> => {
    const result = await call;
    if (result.status === "error") {
      setProblem(result.error);
      return null;
    }
    setProblem(null);
    await reload();
    return result.data;
  };

  const actions: CardActions = {
    duplicate: (id) => void run(commands.duplicatePackage(id)),
    rename: (id, title) => void run(commands.renamePackage(id, title)),
    export: (id) => void run(commands.exportPackage(id)),
    reveal: (id) => void run(commands.revealPackage(id)),
    remove: (id) => void run(commands.deletePackage(id)),
  };

  const importDeck = async () => {
    const imported = await run(commands.importWidget());
    if (imported) void navigate(`/widgets/${imported.id}`);
  };

  return (
    <div className={styles.page}>
      <PageHeader
        title="Widgets"
        actions={
          <>
            <Button onClick={() => void importDeck()}>Import .deck</Button>
            <Button variant="primary" onClick={() => setCreating(true)}>
              New widget
            </Button>
          </>
        }
      />
      {search}

      {library.state === "failed" && <Problem>{describeLibraryError(library.error)}</Problem>}
      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <div className={styles.grid}>
        {shown.map((widget) => (
          <WidgetCard key={widget.id} widget={widget} inUse={active.has(widget.id) ? "Active" : null} use={{ kind: "edit", href: `/widgets/${widget.id}`, actions }} />
        ))}
        <button type="button" className={styles.newTile} onClick={() => setCreating(true)}>
          <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14 6v16M6 14h16" />
          </svg>
          New widget
        </button>
      </div>
      {noMatch}
      {creating && <NewWidgetDialog onClose={() => setCreating(false)} />}
    </div>
  );
}

/** The library in select mode, from the Overview's +: picking one places it in the draft's first free spot. */
export function AddWidget() {
  const navigate = useNavigate();
  const draft = useSeededDraft();
  const { library } = useWidgets();
  const [problem, setProblem] = useState<string | null>(null);
  const placed = useMemo(() => new Set((draft?.widgets ?? []).map((w) => w.package)), [draft]);
  const { shown, search, noMatch } = useShown(library.state === "ready" ? library.widgets : [], placed, "On the keyboard");

  const pick = (widget: WidgetSummary) => {
    const index = placeWidget(widget);
    if (index === null) return setProblem(`There's no room for ${widget.title} in the widget area`);
    void navigate("/", { state: { selectWidget: index } satisfies AddedWidget });
  };

  return (
    <div className={styles.page}>
      <PageHeader back={{ to: "/", label: "Overview" }} title="Add a widget" />
      {search}
      {library.state === "failed" && <Problem>{describeLibraryError(library.error)}</Problem>}
      {problem && <Problem>{problem}</Problem>}
      <div className={styles.grid}>
        {shown.map((widget) => (
          <WidgetCard key={widget.id} widget={widget} inUse={placed.has(widget.id) ? "On the keyboard" : null} use={{ kind: "pick", onPick: () => pick(widget) }} />
        ))}
      </div>
      {noMatch}
    </div>
  );
}
