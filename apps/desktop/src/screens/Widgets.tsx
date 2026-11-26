import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { commands, type LibraryError, type WidgetSummary } from "../bindings";
import { Button, ButtonLink } from "../components/Button";
import { Input } from "../components/Input";
import { Menu, MenuDivider, MenuItem, MenuLink } from "../components/Menu";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { Tag } from "../components/Tag";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError, sizeLabel } from "../library";
import styles from "./Widgets.module.css";

type Library = { state: "loading" } | { state: "ready"; widgets: WidgetSummary[] } | { state: "failed"; error: LibraryError };
type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };

const PREVIEW_SCALE = 0.4;
const PREVIEW_ROOM = { w: 236, h: 110 };

function footprint({ w, h }: { w: number; h: number }) {
  const scale = Math.min(PREVIEW_SCALE, PREVIEW_ROOM.w / w, PREVIEW_ROOM.h / h);
  return { width: w * scale, height: h * scale };
}

function HostIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="#8dbcff" strokeWidth="2">
      <rect x="2" y="3" width="18" height="12" rx="2" />
      <path d="M8 19h6M11 15v4" />
    </svg>
  );
}

function DotsIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true" fill="currentColor">
      <circle cx="4" cy="9" r="1.6" />
      <circle cx="9" cy="9" r="1.6" />
      <circle cx="14" cy="9" r="1.6" />
    </svg>
  );
}

type Actions = {
  duplicate: (id: string) => void;
  rename: (id: string, title: string) => void;
  export: (id: string) => void;
  reveal: (id: string) => void;
  remove: (id: string) => void;
};

function WidgetCard(props: { widget: WidgetSummary; active: boolean; menuOpen: boolean; onMenu: (open: boolean) => void; actions: Actions }) {
  const { widget, active, actions, menuOpen, onMenu } = props;
  const [renaming, setRenaming] = useState(false);
  const [title, setTitle] = useState(widget.title);
  const slotRef = useRef<HTMLDivElement>(null);
  const editor = `/widgets/${widget.id}`;

  useEffect(() => {
    if (!menuOpen) return;
    const close = (e: PointerEvent) => {
      if (!slotRef.current?.contains(e.target as Node)) onMenu(false);
    };
    window.addEventListener("pointerdown", close);
    return () => window.removeEventListener("pointerdown", close);
  }, [menuOpen, onMenu]);

  const act = (run: () => void) => () => {
    onMenu(false);
    run();
  };

  const finishRename = () => {
    setRenaming(false);
    if (title.trim() && title !== widget.title) actions.rename(widget.id, title);
    else setTitle(widget.title);
  };

  const body = (
    <>
      <div className={styles.preview}>
        <div className={styles.footprint} style={footprint(widget.sizing.size)} />
      </div>
      <div className={styles.body}>
        <div className={styles.nameRow}>
          {renaming ? (
            <Input
              variant="inline"
              autoFocus
              aria-label="Title"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onBlur={finishRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") finishRename();
                if (e.key === "Escape") {
                  setTitle(widget.title);
                  setRenaming(false);
                }
              }}
            />
          ) : (
            <span className={styles.name}>{widget.title}</span>
          )}
          <span className={styles.small}>{widget.version}</span>
        </div>
        <span className={styles.small}>{widget.id}</span>
        <div className={styles.tags}>
          <Tag>{widget.sizing.kind === "any" ? "Any size" : sizeLabel(widget.sizing)}</Tag>
          {widget.runs_on_host && (
            <Tag tone="host" role="img" aria-label="Runs on this device" title="Runs on this device">
              <HostIcon />
            </Tag>
          )}
          {active && <Tag tone="active">Active</Tag>}
        </div>
      </div>
    </>
  );

  return (
    <div className={styles.slot} ref={slotRef}>
      {renaming ? (
        <div className={styles.card}>{body}</div>
      ) : (
        <Link className={styles.card} to={editor}>
          {body}
        </Link>
      )}
      <button
        type="button"
        className={styles.more}
        aria-label={`More actions for ${widget.title}`}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onClick={() => onMenu(!menuOpen)}
      >
        <DotsIcon />
      </button>
      {menuOpen && (
        <Menu className={styles.menu}>
          <MenuLink to={editor}>Edit</MenuLink>
          <MenuItem onClick={act(() => actions.duplicate(widget.id))}>Duplicate</MenuItem>
          <MenuItem onClick={act(() => setRenaming(true))}>Rename</MenuItem>
          <MenuItem onClick={act(() => actions.export(widget.id))}>Export .deck</MenuItem>
          <MenuItem onClick={act(() => actions.reveal(widget.id))}>Show in folder</MenuItem>
          <MenuDivider />
          {active ? (
            <MenuItem disabled>Delete (active)</MenuItem>
          ) : (
            <MenuItem danger onClick={act(() => actions.remove(widget.id))}>
              Delete
            </MenuItem>
          )}
        </Menu>
      )}
    </div>
  );
}

export function Widgets() {
  const navigate = useNavigate();
  const theme = useGlassdeckd((s) => s.theme);
  const [library, setLibrary] = useState<Library>({ state: "loading" });
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [query, setQuery] = useState("");
  const [openMenu, setOpenMenu] = useState<string | null>(null);
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

  const actions: Actions = {
    duplicate: (id) => void run(commands.duplicateWidget(id)),
    rename: (id, title) => void run(commands.renameWidget(id, title)),
    export: (id) => void run(commands.exportWidget(id)),
    reveal: (id) => void run(commands.revealWidget(id)),
    remove: (id) => void run(commands.deleteWidget(id)),
  };

  const importDeck = async () => {
    const imported = await run(commands.importWidget());
    if (imported) navigate(`/widgets/${imported.id}`);
  };

  const shown =
    library.state === "ready"
      ? library.widgets.filter((w) => `${w.title} ${w.id}`.toLowerCase().includes(query.trim().toLowerCase()))
      : [];

  return (
    <div className={styles.page}>
      <PageHeader
        title="Widgets"
        actions={
          <>
            <Input type="search" variant="search" placeholder="Search widgets" aria-label="Search widgets" value={query} onChange={(e) => setQuery(e.target.value)} />
            <Button onClick={() => void importDeck()}>Import .deck</Button>
            <ButtonLink variant="primary" to="/widgets/new">
              New widget
            </ButtonLink>
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
            menuOpen={openMenu === widget.id}
            onMenu={(open) => setOpenMenu(open ? widget.id : null)}
            actions={actions}
          />
        ))}
        <Link className={styles.newTile} to="/widgets/new">
          <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14 6v16M6 14h16" />
          </svg>
          New widget
        </Link>
      </div>
      {library.state === "ready" && library.widgets.length > 0 && shown.length === 0 && <div className={styles.noMatch}>No widgets match “{query}”.</div>}
    </div>
  );
}
