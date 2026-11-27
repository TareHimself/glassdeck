import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { commands, type LibraryError, type WidgetSummary } from "../bindings";
import { Button } from "../components/Button";
import { Input } from "../components/Input";
import { LibraryCard, type CardActions } from "../components/LibraryCard";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { Tag } from "../components/Tag";
import { WidgetThumb } from "../components/Thumbs";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError, sizeLabel } from "../library";
import { matchesQuery } from "../search";
import { NewWidgetDialog } from "./NewWidgetDialog";
import styles from "./Widgets.module.css";

type Library = { state: "loading" } | { state: "ready"; widgets: WidgetSummary[] } | { state: "failed"; error: LibraryError };
type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };

function HostIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="#8dbcff" strokeWidth="2">
      <rect x="2" y="3" width="18" height="12" rx="2" />
      <path d="M8 19h6M11 15v4" />
    </svg>
  );
}

function WidgetCard(props: { widget: WidgetSummary; active: boolean; actions: CardActions }) {
  const { widget } = props;
  return (
    <LibraryCard
      {...props}
      id={widget.id}
      title={widget.title}
      version={widget.version}
      href={`/widgets/${widget.id}`}
      preview={<WidgetThumb className={styles.preview} widget={widget} />}
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

export function Widgets() {
  const navigate = useNavigate();
  const theme = useGlassdeckd((s) => s.theme);
  const [library, setLibrary] = useState<Library>({ state: "loading" });
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const active = useMemo(() => new Set((theme?.widgets ?? []).map((w) => w.package)), [theme]);

  const reload = async () => {
    const result = await commands.listWidgets();
    setLibrary(result.status === "ok" ? { state: "ready", widgets: result.data } : { state: "failed", error: result.error });
  };

  useEffect(() => {
    void reload();
  }, []);

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
    if (imported) navigate(`/widgets/${imported.id}`);
  };

  const shown =
    library.state === "ready"
      ? library.widgets.filter((w) => matchesQuery(query, w.title, w.id))
      : [];

  return (
    <div className={styles.page}>
      <PageHeader
        title="Widgets"
        actions={
          <>
            <Input type="search" variant="search" placeholder="Search widgets" aria-label="Search widgets" value={query} onChange={(e) => setQuery(e.target.value)} />
            <Button onClick={() => void importDeck()}>Import .deck</Button>
            <Button variant="primary" onClick={() => setCreating(true)}>
              New widget
            </Button>
          </>
        }
      />

      {library.state === "failed" && <Problem>{describeLibraryError(library.error)}</Problem>}
      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <div className={styles.grid}>
        {shown.map((widget) => (
          <WidgetCard
            key={widget.id}
            widget={widget}
            active={active.has(widget.id)}
            actions={actions}
          />
        ))}
        <button type="button" className={styles.newTile} onClick={() => setCreating(true)}>
          <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14 6v16M6 14h16" />
          </svg>
          New widget
        </button>
      </div>
      {library.state === "ready" && library.widgets.length > 0 && shown.length === 0 && <div className={styles.noMatch}>No widgets match “{query}”.</div>}
      {creating && <NewWidgetDialog onClose={() => setCreating(false)} />}
    </div>
  );
}
