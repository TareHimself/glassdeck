import { useCallback, useRef, useState, type ReactNode } from "react";
import { Link } from "react-router";
import { Input } from "./Input";
import { Menu, MenuDivider, MenuItem, MenuLink } from "./Menu";
import { Tag } from "./Tag";
import { useClickOutside } from "./useClickOutside";
import styles from "./LibraryCard.module.css";

export type CardActions = {
  duplicate: (id: string) => void;
  rename: (id: string, title: string) => void;
  export: (id: string) => void;
  reveal: (id: string) => void;
  remove: (id: string) => void;
};

type Props = {
  id: string;
  title: string;
  version: string;
  href: string;
  preview: ReactNode;
  tags?: ReactNode;
  active: boolean;
  menuOpen: boolean;
  onMenu: (open: boolean) => void;
  actions: CardActions;
};

function DotsIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true" fill="currentColor">
      <circle cx="4" cy="9" r="1.6" />
      <circle cx="9" cy="9" r="1.6" />
      <circle cx="14" cy="9" r="1.6" />
    </svg>
  );
}

/** A widget or background in the library: its preview, title, id and tags, with the ⋯ menu. */
export function LibraryCard({ id, title, version, href, preview, tags, active, menuOpen, onMenu, actions }: Props) {
  const [renaming, setRenaming] = useState(false);
  const [draft, setDraft] = useState(title);
  const slotRef = useRef<HTMLDivElement>(null);

  useClickOutside(menuOpen, slotRef, useCallback(() => onMenu(false), [onMenu]));

  const act = (run: () => void) => () => {
    onMenu(false);
    run();
  };

  const finishRename = () => {
    setRenaming(false);
    if (draft.trim() && draft !== title) actions.rename(id, draft);
    else setDraft(title);
  };

  const body = (
    <>
      {preview}
      <div className={styles.body}>
        <div className={styles.nameRow}>
          {renaming ? (
            <Input
              variant="inline"
              autoFocus
              aria-label="Title"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={finishRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") finishRename();
                if (e.key === "Escape") {
                  setDraft(title);
                  setRenaming(false);
                }
              }}
            />
          ) : (
            <span className={styles.name}>{title}</span>
          )}
          <span className={styles.small}>{version}</span>
        </div>
        <span className={styles.small}>{id}</span>
        <div className={styles.tags}>
          {tags}
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
        <Link className={styles.card} to={href}>
          {body}
        </Link>
      )}
      <button type="button" className={styles.more} aria-label={`More actions for ${title}`} aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => onMenu(!menuOpen)}>
        <DotsIcon />
      </button>
      {menuOpen && (
        <Menu className={styles.menu}>
          <MenuLink to={href}>Edit</MenuLink>
          <MenuItem onClick={act(() => actions.duplicate(id))}>Duplicate</MenuItem>
          <MenuItem onClick={act(() => setRenaming(true))}>Rename</MenuItem>
          <MenuItem onClick={act(() => actions.export(id))}>Export .deck</MenuItem>
          <MenuItem onClick={act(() => actions.reveal(id))}>Show in folder</MenuItem>
          <MenuDivider />
          {active ? (
            <MenuItem disabled>Delete (active)</MenuItem>
          ) : (
            <MenuItem danger onClick={act(() => actions.remove(id))}>
              Delete
            </MenuItem>
          )}
        </Menu>
      )}
    </div>
  );
}
