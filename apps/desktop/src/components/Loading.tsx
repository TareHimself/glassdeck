import styles from "./Loading.module.css";

/** Over a preview while something for it loads: a spinner and what's happening. */
export function Loading({ label }: { label: string }) {
  return (
    <div className={styles.overlay} role="status">
      <span className={styles.spinner} aria-hidden="true" />
      {label}
    </div>
  );
}
