import type { ReactNode } from "react";
import { Link } from "react-router";
import styles from "./PageHeader.module.css";

export function PageHeader(props: { title: ReactNode; back?: { to: string; label: string }; id?: string; actions?: ReactNode }) {
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
