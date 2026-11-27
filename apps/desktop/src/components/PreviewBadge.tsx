import styles from "./PreviewBadge.module.css";

/** On a hovered card: a spinning ring round a sparkle while its preview starts, then a steady sparkle while it plays. */
export function PreviewBadge({ playing }: { playing: boolean }) {
  return playing ? (
    <span className={styles.badge} data-playing role="img" aria-label="Previewing" title="Previewing">
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
        <path d="M7 2c.5 3.3 2.2 5 5.5 5.5C9.2 8 7.5 9.7 7 13c-.5-3.3-2.2-5-5.5-5.5C4.8 7 6.5 5.3 7 2z" />
        <path d="M12.5 9.5c.2 1.4.9 2.1 2.3 2.3-1.4.2-2.1.9-2.3 2.3-.2-1.4-.9-2.1-2.3-2.3 1.4-.2 2.1-.9 2.3-2.3z" opacity=".8" />
      </svg>
    </span>
  ) : (
    <span className={styles.badge} role="status" aria-label="Starting the preview" title="Starting the preview">
      <span className={styles.ring} />
      <svg className={styles.twinkle} width="12" height="12" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
        <path d="M8 1c.5 3.6 2.4 5.5 6 7-3.6 1.5-5.5 3.4-6 7-.5-3.6-2.4-5.5-6-7 3.6-1.5 5.5-3.4 6-7z" />
      </svg>
    </span>
  );
}
