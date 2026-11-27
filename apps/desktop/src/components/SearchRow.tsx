import type { ReactNode } from "react";
import { Input } from "./Input";
import styles from "./SearchRow.module.css";

/** A library page's search, with its filters beside it. */
export function SearchRow({ label, query, onQuery, children }: { label: string; query: string; onQuery: (query: string) => void; children?: ReactNode }) {
  return (
    <div className={styles.row}>
      <Input type="search" variant="search" className={styles.search} placeholder="Search" aria-label={label} value={query} onChange={(e) => onQuery(e.target.value)} />
      {children}
    </div>
  );
}
