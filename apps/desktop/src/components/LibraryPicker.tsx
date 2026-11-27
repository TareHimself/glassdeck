import { useState, type ReactNode } from "react";
import { matchesQuery } from "../search";
import { Button } from "./Button";
import { Dialog } from "./Dialog";
import { Input } from "./Input";
import styles from "./LibraryPicker.module.css";

export type PickerCard = {
  title: string;
  id: string;
  /** Beside the title, such as a kind icon or a size */
  detail?: ReactNode;
  current?: boolean;
};

type Props<T> = {
  title: string;
  searchLabel: string;
  items: T[];
  keyOf: (item: T) => string;
  searchText: (item: T) => string[];
  card: (item: T) => PickerCard;
  /** The card's picture, playing while `live` (hovered or focused) */
  preview: (item: T, live: boolean) => ReactNode;
  newLabel: string;
  onNew: () => void;
  onPick: (item: T) => void;
  onClose: () => void;
};

/** Pick one thing from the library: search, a grid of cards that play on hover, and a way to make a new one. */
export function LibraryPicker<T>({ title, searchLabel, items, keyOf, searchText, card, preview, newLabel, onNew, onPick, onClose }: Props<T>) {
  const [query, setQuery] = useState("");
  const [live, setLive] = useState<string | null>(null);
  const shown = items.filter((item) => matchesQuery(query, ...searchText(item)));

  return (
    <Dialog title={title} width={960} onClose={onClose}>
      <Input type="search" variant="search" className={styles.search} placeholder="Search" aria-label={searchLabel} autoFocus value={query} onChange={(e) => setQuery(e.target.value)} />
      <div className={styles.grid}>
        {shown.map((item) => {
          const key = keyOf(item);
          const { title: name, id, detail, current } = card(item);
          const start = () => setLive(key);
          const stop = () => setLive((was) => (was === key ? null : was));
          return (
            <button
              key={key}
              type="button"
              className={styles.card}
              aria-pressed={current}
              onClick={() => onPick(item)}
              onPointerEnter={start}
              onPointerLeave={stop}
              onFocus={start}
              onBlur={stop}
            >
              <div className={styles.preview}>
                {preview(item, live === key)}
                {current && <span className={styles.current}>Current</span>}
              </div>
              <div className={styles.text}>
                <div className={styles.titleRow}>
                  <span className={styles.title}>{name}</span>
                  {detail}
                </div>
                <span className={styles.id}>{id}</span>
              </div>
            </button>
          );
        })}
        {shown.length === 0 && <div className={styles.none}>No matches</div>}
      </div>
      <div className={styles.footer}>
        <Button onClick={onNew}>{newLabel}</Button>
      </div>
    </Dialog>
  );
}
