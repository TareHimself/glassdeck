import styles from "./PreviewBadge.module.css";

/** On a hovered card: a spinning arc while its preview starts, then a play mark with a ring pulsing out while it plays. */
export function PreviewBadge({ playing }: { playing: boolean }) {
  return playing ? (
    <span className={styles.badge} role="img" aria-label="Previewing" title="Previewing">
      <span className={styles.pulse} />
      <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true" fill="currentColor">
        <path d="M3 1.5v9l7.5-4.5z" />
      </svg>
    </span>
  ) : (
    <span className={styles.badge} role="status" aria-label="Starting the preview" title="Starting the preview">
      <svg className={styles.spinner} width="22" height="22" viewBox="0 0 22 22" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
        <circle cx="11" cy="11" r="9" opacity="0.22" />
        <path d="M11 2a9 9 0 0 1 9 9" strokeLinecap="round" />
      </svg>
    </span>
  );
}
