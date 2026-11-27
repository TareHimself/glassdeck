import { useRef, useState, type ReactNode, type RefObject } from "react";
import { Link } from "react-router";
import { useHoverPlay } from "./hoverPlay";
import { Input } from "./Input";
import { Menu, MenuDivider, MenuItem, MenuLink } from "./Menu";
import { Tag } from "./Tag";
import styles from "./LibraryCard.module.css";

export type CardActions = {
  duplicate: (id: string) => void;
  rename: (id: string, title: string) => void;
  export: (id: string) => void;
  reveal: (id: string) => void;
  remove: (id: string) => void;
};

/** What a card does: open its editor, with the ⋯ menu, or be picked (the library in select mode). */
export type CardUse = { kind: "edit"; href: string; actions: CardActions } | { kind: "pick"; onPick: () => void };

type Props = {
  id: string;
  title: string;
  version: string;
  /** The picture, playing while `live` (hovered or focused for a moment) */
  preview: (live: boolean) => ReactNode;
  tags?: ReactNode;
  /** The tag for one in use, such as "Active", or none */
  inUse: string | null;
  use: CardUse;
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

/** A widget or background in the library: its preview, title and tags; opened to edit, or picked. */
export function LibraryCard({ id, title, version, preview, tags, inUse, use }: Props) {
  const { live, start, stop } = useHoverPlay();
  const [renaming, setRenaming] = useState(false);
  const renameField = useRef<HTMLInputElement>(null);
  const body = (name: ReactNode) => (
    <>
      {preview(live === id)}
      <div className={styles.body}>
        <div className={styles.nameRow}>
          {name}
          <span className={styles.small}>{version}</span>
        </div>
        <div className={styles.tags}>
          {tags}
          {inUse && <Tag tone="active">{inUse}</Tag>}
        </div>
      </div>
    </>
  );
  const name = <span className={styles.name}>{title}</span>;

  return (
    <div className={styles.slot} onPointerEnter={() => start(id)} onPointerLeave={() => stop(id)} onFocus={() => start(id)} onBlur={() => stop(id)}>
      {use.kind === "pick" ? (
        <button type="button" className={styles.card} onClick={use.onPick}>
          {body(name)}
        </button>
      ) : renaming ? (
        <div className={styles.card}>{body(<RenameField field={renameField} id={id} title={title} actions={use.actions} onDone={() => setRenaming(false)} />)}</div>
      ) : (
        <Link className={styles.card} to={use.href}>
          {body(name)}
        </Link>
      )}
      {use.kind === "edit" && <CardMenu id={id} title={title} href={use.href} actions={use.actions} inUse={inUse !== null} onRename={() => setRenaming(true)} onRenameShown={() => renameField.current?.select()} />}
    </div>
  );
}

function RenameField({ field, id, title, actions, onDone }: { field: RefObject<HTMLInputElement | null>; id: string; title: string; actions: CardActions; onDone: () => void }) {
  const [draft, setDraft] = useState(title);
  const finish = () => {
    onDone();
    if (draft.trim() && draft !== title) actions.rename(id, draft);
  };
  return (
    <Input
      ref={field}
      variant="inline"
      aria-label="Title"
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={finish}
      onKeyDown={(e) => {
        if (e.key === "Enter") finish();
        if (e.key === "Escape") onDone();
      }}
    />
  );
}

type CardMenuProps = { id: string; title: string; href: string; actions: CardActions; inUse: boolean; onRename: () => void; onRenameShown: () => void };

/** The ⋯ menu. After Rename it hands focus to the title field once it has closed, rather than back to ⋯. */
function CardMenu({ id, title, href, actions, inUse, onRename, onRenameShown }: CardMenuProps) {
  const renaming = useRef(false);
  return (
    <Menu
      onCloseAutoFocus={(e) => {
        if (!renaming.current) return;
        renaming.current = false;
        e.preventDefault();
        onRenameShown();
      }}
      trigger={
        <button type="button" className={styles.more} aria-label={`More actions for ${title}`}>
          <DotsIcon />
        </button>
      }
    >
      <MenuLink to={href}>Edit</MenuLink>
      <MenuItem onSelect={() => actions.duplicate(id)}>Duplicate</MenuItem>
      <MenuItem
        onSelect={() => {
          renaming.current = true;
          onRename();
        }}
      >
        Rename
      </MenuItem>
      <MenuItem onSelect={() => actions.export(id)}>Export .deck</MenuItem>
      <MenuItem onSelect={() => actions.reveal(id)}>Show in folder</MenuItem>
      <MenuDivider />
      {inUse ? (
        <MenuItem disabled>Delete (active)</MenuItem>
      ) : (
        <MenuItem danger onSelect={() => actions.remove(id)}>
          Delete
        </MenuItem>
      )}
    </Menu>
  );
}
