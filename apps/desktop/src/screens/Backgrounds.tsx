import { useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { commands, type BackgroundKind, type BackgroundSummary, type LibraryError } from "../bindings";
import { Button } from "../components/Button";
import { LibraryCard, type CardActions, type CardUse } from "../components/LibraryCard";
import { BackgroundThumb } from "../components/Thumbs";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { SearchRow } from "../components/SearchRow";
import { Segments } from "../components/Segments";
import { setDraft, useSeededDraft } from "../draft";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError, useBuiltInBackground } from "../library";
import { matchesQuery } from "../search";
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

function BackgroundCard({ background, inUse, use }: { background: BackgroundSummary; inUse: string | null; use: CardUse }) {
  return (
    <LibraryCard
      id={background.id}
      title={background.title}
      version={background.version}
      inUse={inUse}
      use={use}
      preview={(live) => (
        <BackgroundThumb className={styles.thumb} background={background} live={live} badge>
          <span className={styles.kind} data-kind={background.kind}>
            {KIND_NAMES[background.kind]}
          </span>
        </BackgroundThumb>
      )}
    />
  );
}

function useBackgrounds() {
  const [library, setLibrary] = useState<Library>({ state: "loading" });
  const reload = async () => {
    const result = await commands.listBackgrounds();
    setLibrary(result.status === "ok" ? { state: "ready", backgrounds: result.data } : { state: "failed", error: result.error });
  };
  useEffect(() => {
    void reload();
  }, []);
  return { library, reload };
}

function useShown(backgrounds: BackgroundSummary[]) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const shown = backgrounds.filter((b) => (filter === "all" || b.kind === filter) && matchesQuery(query, b.title));
  const search = (
    <SearchRow label="Search backgrounds" query={query} onQuery={setQuery}>
      <Segments compact label="Filter by type" options={FILTERS} value={filter} onChange={setFilter} />
    </SearchRow>
  );
  return { shown, search };
}

/** The background library: each one to open in its editor, and new ones made here. */
export function Backgrounds() {
  const theme = useGlassdeckd((s) => s.theme);
  const { library, reload } = useBackgrounds();
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);
  const { shown, search } = useShown(library.state === "ready" ? library.backgrounds : []);

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

  return (
    <div className={styles.page}>
      <PageHeader
        title="Backgrounds"
        actions={
          <Button variant="primary" onClick={() => setCreating(true)}>
            New background
          </Button>
        }
      />
      {search}

      {library.state === "failed" && <Problem>{describeLibraryError(library.error)}</Problem>}
      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <div className={styles.grid}>
        {shown.map((background) => (
          <BackgroundCard
            key={background.id}
            background={background}
            inUse={theme?.background.package === background.id ? "Active" : null}
            use={{ kind: "edit", href: `/backgrounds/${background.id}`, actions }}
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

/** The library in select mode, from the Overview's Change: picking one makes it the draft's background. */
export function ChooseBackground() {
  const navigate = useNavigate();
  const draft = useSeededDraft();
  const builtIn = useBuiltInBackground();
  const { library } = useBackgrounds();
  const { shown, search } = useShown(library.state === "ready" ? [builtIn, ...library.backgrounds] : []);
  const current = draft?.background.package;

  const pick = (background: BackgroundSummary) => {
    if (background.id !== current) setDraft((d) => ({ ...d, background: { package: background.id, options: {} } }));
    void navigate("/");
  };

  return (
    <div className={styles.page}>
      <PageHeader back={{ to: "/", label: "Overview" }} title="Choose a background" />
      {search}
      {library.state === "failed" && <Problem>{describeLibraryError(library.error)}</Problem>}
      <div className={styles.grid}>
        {shown.map((background) => (
          <BackgroundCard
            key={background.id}
            background={background}
            inUse={background.id === current ? "Current" : null}
            use={{ kind: "pick", onPick: () => pick(background) }}
          />
        ))}
      </div>
    </div>
  );
}
