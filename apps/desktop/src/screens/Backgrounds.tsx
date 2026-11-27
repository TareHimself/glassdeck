import { useEffect, useState } from "react";
import { Link } from "react-router";
import { commands, type BackgroundKind, type BackgroundSummary, type LibraryError } from "../bindings";
import { ButtonLink } from "../components/Button";
import { LibraryCard, type CardActions } from "../components/LibraryCard";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { Segments } from "../components/Segments";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError } from "../library";
import { KIND_NAMES } from "./backgroundKinds";
import styles from "./Backgrounds.module.css";

type Library = { state: "loading" } | { state: "ready"; backgrounds: BackgroundSummary[] } | { state: "failed"; error: LibraryError };
type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };
type Filter = "all" | BackgroundKind;

const FILTERS: { value: Filter; name: string }[] = [
  { value: "all", name: "All" },
  { value: "canvas", name: KIND_NAMES.canvas },
  { value: "video", name: KIND_NAMES.video },
];

function BackgroundCard(props: { background: BackgroundSummary; active: boolean; menuOpen: boolean; onMenu: (open: boolean) => void; actions: CardActions }) {
  const { background } = props;
  return (
    <LibraryCard
      {...props}
      id={background.id}
      title={background.title}
      version={background.version}
      href={`/backgrounds/${background.id}`}
      preview={
        <div className={styles.thumb} data-kind={background.kind}>
          <span className={styles.kind} data-kind={background.kind}>
            {KIND_NAMES[background.kind]}
          </span>
        </div>
      }
    />
  );
}

export function Backgrounds() {
  const theme = useGlassdeckd((s) => s.theme);
  const [library, setLibrary] = useState<Library>({ state: "loading" });
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [openMenu, setOpenMenu] = useState<string | null>(null);

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
            <ButtonLink variant="primary" to="/backgrounds/new">
              New background
            </ButtonLink>
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
            menuOpen={openMenu === background.id}
            onMenu={(open) => setOpenMenu(open ? background.id : null)}
            actions={actions}
          />
        ))}
        <Link className={styles.newTile} to="/backgrounds/new">
          <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14 6v16M6 14h16" />
          </svg>
          New background
        </Link>
      </div>
    </div>
  );
}
