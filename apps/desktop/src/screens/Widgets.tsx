import { css } from "@linaria/core";
import { styled } from "@linaria/react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { commands, type LibraryError, type WidgetSummary } from "../bindings";
import { useGlassdeckd } from "../glassdeckd";
import { describeLibraryError, sizeLabel } from "../library";
import { Input, Problem, Title, primary, secondary } from "../ui";

type Library = { state: "loading" } | { state: "ready"; widgets: WidgetSummary[] } | { state: "failed"; error: LibraryError };

const PREVIEW_SCALE = 0.4;
const PREVIEW_ROOM = { w: 236, h: 110 };

function footprint({ w, h }: { w: number; h: number }) {
  const scale = Math.min(PREVIEW_SCALE, PREVIEW_ROOM.w / w, PREVIEW_ROOM.h / h);
  return { width: w * scale, height: h * scale };
}

const Page = styled.div`
  display: flex;
  flex-direction: column;
  gap: 18px;
`;

const Header = styled.header`
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;

  & > :first-child {
    flex: 1 1 280px;
  }
`;

const Grid = styled.div`
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
  gap: 14px;
`;

const Slot = styled.div`
  position: relative;
`;

const card = css`
  display: flex;
  flex-direction: column;
  border-radius: 12px;
  background: var(--surface);
  border: 1px solid var(--line);
  overflow: hidden;
  color: var(--ink);
  text-decoration: none;

  &:hover {
    color: var(--ink);
  }
`;

const Preview = styled.div`
  height: 130px;
  display: flex;
  align-items: center;
  justify-content: center;
  background:
    radial-gradient(circle at 25% 15%, rgba(214, 107, 120, 0.5), transparent 60%),
    radial-gradient(circle at 85% 95%, rgba(13, 92, 133, 0.7), transparent 55%),
    linear-gradient(180deg, #0f0a29, #3d1c73);
`;

const Footprint = styled.div`
  border-radius: 6px;
  border: 1px dashed rgba(255, 255, 255, 0.4);
`;

const Body = styled.div`
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 6px;
`;

const NameRow = styled.div`
  display: flex;
  align-items: center;
  gap: 8px;
`;

const Name = styled.span`
  flex: 1;
  font-weight: 600;
  font-size: 15px;
`;

const Small = styled.span`
  font-family: var(--mono);
  font-size: 12px;
  color: var(--muted);
  overflow-wrap: anywhere;
`;

const Tags = styled.div`
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-top: 2px;
`;

const Tag = styled.span`
  display: inline-flex;
  align-items: center;
  justify-content: center;
  padding: 2px 8px;
  border-radius: 999px;
  background: #1f2633;
  font-size: 12px;
  white-space: nowrap;

  &[data-tone="host"] {
    width: 26px;
    height: 22px;
    padding: 0;
    background: #1b2b3a;
  }

  &[data-tone="active"] {
    background: #17301f;
    color: #7fe0a8;
  }
`;

const MoreButton = styled.button`
  position: absolute;
  right: 8px;
  top: 8px;
  width: 40px;
  height: 40px;
  border-radius: 8px;
  border: 0;
  background: rgba(8, 10, 14, 0.72);
  color: var(--ink);
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
`;

const Menu = styled.div`
  position: absolute;
  right: 8px;
  top: 52px;
  z-index: 5;
  min-width: 190px;
  padding: 6px;
  border-radius: 10px;
  background: #1b2029;
  border: 1px solid var(--line-strong);
  box-shadow: 0 12px 30px rgba(0, 0, 0, 0.45);
  display: flex;
  flex-direction: column;
`;

const menuItem = css`
  display: flex;
  align-items: center;
  min-height: 40px;
  padding: 0 12px;
  border-radius: 7px;
  border: 0;
  background: transparent;
  color: var(--ink);
  text-align: left;
  text-decoration: none;
  cursor: pointer;

  &:hover {
    background: #232a37;
    color: var(--ink);
  }

  &[data-danger] {
    color: var(--danger);
  }

  &:disabled {
    color: #6b7483;
    cursor: not-allowed;
    background: transparent;
  }
`;

const Divider = styled.div`
  height: 1px;
  background: var(--line-strong);
  margin: 4px 6px;
`;

const newTile = css`
  min-height: 230px;
  text-decoration: none;
  border-radius: 12px;
  border: 1px dashed #343b48;
  color: var(--muted);
  font-weight: 500;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;

  &:hover {
    color: var(--ink);
  }
`;

const NoMatch = styled.div`
  color: var(--muted);
`;

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
      <Preview>
        <Footprint style={footprint(widget.sizing.size)} />
      </Preview>
      <Body>
        <NameRow>
          {renaming ? (
            <Input
              data-variant="inline"
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
            <Name>{widget.title}</Name>
          )}
          <Small>{widget.version}</Small>
        </NameRow>
        <Small>{widget.id}</Small>
        <Tags>
          <Tag>{widget.sizing.kind === "any" ? "Any size" : sizeLabel(widget.sizing)}</Tag>
          {widget.runs_on_host && (
            <Tag data-tone="host" role="img" aria-label="Runs on this device" title="Runs on this device">
              <HostIcon />
            </Tag>
          )}
          {active && <Tag data-tone="active">Active</Tag>}
        </Tags>
      </Body>
    </>
  );

  return (
    <Slot ref={slotRef}>
      {renaming ? (
        <div className={card}>{body}</div>
      ) : (
        <Link className={card} to={editor}>
          {body}
        </Link>
      )}
      <MoreButton type="button" aria-label={`More actions for ${widget.title}`} aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => onMenu(!menuOpen)}>
        <DotsIcon />
      </MoreButton>
      {menuOpen && (
        <Menu role="menu">
          <Link role="menuitem" className={menuItem} to={editor}>
            Edit
          </Link>
          <button type="button" role="menuitem" className={menuItem} onClick={act(() => actions.duplicate(widget.id))}>
            Duplicate
          </button>
          <button type="button" role="menuitem" className={menuItem} onClick={act(() => setRenaming(true))}>
            Rename
          </button>
          <button type="button" role="menuitem" className={menuItem} onClick={act(() => actions.export(widget.id))}>
            Export .deck
          </button>
          <button type="button" role="menuitem" className={menuItem} onClick={act(() => actions.reveal(widget.id))}>
            Show in folder
          </button>
          <Divider />
          {active ? (
            <button type="button" role="menuitem" className={menuItem} disabled>
              Delete (active)
            </button>
          ) : (
            <button type="button" role="menuitem" className={menuItem} data-danger onClick={act(() => actions.remove(widget.id))}>
              Delete
            </button>
          )}
        </Menu>
      )}
    </Slot>
  );
}

type Answer<T> = { status: "ok"; data: T } | { status: "error"; error: LibraryError };

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
    <Page>
      <Header>
        <div>
          <Title>Widgets</Title>
        </div>
        <Input type="search" data-variant="search" placeholder="Search widgets" aria-label="Search widgets" value={query} onChange={(e) => setQuery(e.target.value)} />
        <button type="button" className={secondary} onClick={() => void importDeck()}>
          Import .deck
        </button>
        <Link className={primary} to="/widgets/new">
          New widget
        </Link>
      </Header>

      {library.state === "failed" && <Problem role="alert">{describeLibraryError(library.error)}</Problem>}
      {problem && <Problem role="alert">{describeLibraryError(problem)}</Problem>}

      <Grid>
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
        <Link className={newTile} to="/widgets/new">
          <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14 6v16M6 14h16" />
          </svg>
          New widget
        </Link>
      </Grid>
      {library.state === "ready" && library.widgets.length > 0 && shown.length === 0 && <NoMatch>No widgets match “{query}”.</NoMatch>}
    </Page>
  );
}
