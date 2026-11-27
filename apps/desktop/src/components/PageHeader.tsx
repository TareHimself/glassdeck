import type { ReactNode } from "react";
import { Link, useLocation } from "react-router";
import styles from "./PageHeader.module.css";

export type Back = { to: string; label: string };

/** Where a page opened from another one goes back to: that page, when the link said so, else `fallback`. */
export function useBack(fallback: Back): Back {
  const state: unknown = useLocation().state;
  if (typeof state === "object" && state !== null && "back" in state) {
    const back: unknown = state.back;
    if (typeof back === "object" && back !== null && "to" in back && "label" in back && typeof back.to === "string" && typeof back.label === "string") {
      return { to: back.to, label: back.label };
    }
  }
  return fallback;
}

/** Link state that makes the opened page's back link return to the Overview. */
export const BACK_TO_OVERVIEW = { back: { to: "/", label: "Overview" } satisfies Back };

export function PageHeader(props: { title: ReactNode; back?: Back; id?: string; actions?: ReactNode }) {
  return (
    <header className={styles.header}>
      <div className={styles.heading}>
        {props.back && (
          <Link className={styles.back} to={props.back.to}>
            ← {props.back.label}
          </Link>
        )}
        <h1 className={styles.title}>{props.title}</h1>
        {props.id && <div className={styles.subtitle}>{props.id}</div>}
      </div>
      {props.actions}
    </header>
  );
}
