import { useEffect, useState } from "react";
import { commands, type BackgroundKind, type BackgroundSummary, type LibraryError } from "../bindings";
import { Button } from "../components/Button";
import { LibraryCard, type CardActions } from "../components/LibraryCard";
import { BackgroundThumb } from "../components/Thumbs";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { Segments } from "../components/Segments";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError } from "../library";
import { KIND_NAMES } from "./backgroundKinds";
import { NewBackgroundDialog } from "./NewBackgroundDialog";
import styles from "./Backgrounds.module.css";

type Library = { state: "loading" } | { state: "ready"; backgrounds: BackgroundSummary[] } | { state: "failed"; error: LibraryError };
type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };
type Filter = "all" | BackgroundKind;

const FILTERS: { value: Filter; name: string }[] = [
  { value: "all", name: "All" },
  { value: "canvas", name: KIND_NAMES.canvas },
  { value: "video", name: KIND_NAMES.video },
];

function BackgroundCard(props: { background: BackgroundSummary; active: boolean; actions: CardActions }) {
  const { background } = props;
  return (
    <LibraryCard
      {...props}
      id={background.id}
      title={background.title}
      version={background.version}
      href={`/backgrounds/${background.id}`}
      preview={(live) => (
        <BackgroundThumb className={styles.thumb} background={background} live={live}>
          <span className={styles.kind} data-kind={background.kind}>
            {KIND_NAMES[background.kind]}
          </span>
        </BackgroundThumb>
      )}
    />
  );
}

export function Backgrounds() {
  const theme = useGlassdeckd((s) => s.theme);
  const [library, setLibrary] = useState<Library>({ state: "loading" });
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [creating, setCreating] = useState(false);

  const reload = async () => {
    const result = await commands.listBackgrounds();
    setLibrary(result.status === "ok" ? { state: "ready", backgrounds: result.data } : { state: "failed", error: result.error });
  };

  useEffect(() => {
    void reload();
  }, []);

  const run = async <T,>(call: Promise<Answer<T>>) => {
    const result = await call;
    setProblem(result.status === "error" ? result.error : null);
    await reload();
  };

  const actions: CardActions = {
    duplicate: (id) => void run(commands.duplicatePackage(id)),
    rename: (id, title) => void run(commands.renamePackage(id, title)),
    export: (id) => void run(commands.exportPackage(id)),
    reveal: (id) => void run(commands.revealPackage(id)),
    remove: (id) => void run(commands.deletePackage(id)),
  };

  const shown = library.state === "ready" ? library.backgrounds.filter((b) => filter === "all" || b.kind === filter) : [];

  return (
    <div className={styles.page}>
      <PageHeader
        title="Backgrounds"
        actions={
          <>
            <Segments compact label="Filter by type" options={FILTERS} value={filter} onChange={setFilter} />
            <Button variant="primary" onClick={() => setCreating(true)}>
              New background
            </Button>
          </>
        }
      />

      {library.state === "failed" && <Problem>{describeLibraryError(library.error)}</Problem>}
      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <div className={styles.grid}>
        {shown.map((background) => (
          <BackgroundCard
            key={background.id}
            background={background}
            active={theme?.background.package === background.id}
            actions={actions}
          />
        ))}
        <button type="button" className={styles.newTile} onClick={() => setCreating(true)}>
          <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14 6v16M6 14h16" />
          </svg>
          New background
        </button>
      </div>
      {creating && <NewBackgroundDialog onClose={() => setCreating(false)} />}
    </div>
  );
}
