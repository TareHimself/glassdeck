import { useEffect, useRef, type ReactNode } from "react";
import styles from "./Dialog.module.css";

type Props = {
  title: string;
  width: number;
  onClose: () => void;
  /** Controls beside the title, such as a type switch */
  header?: ReactNode;
  children: ReactNode;
};

/** A dialog over the page: Escape, the close button or a click outside it dismisses it. */
export function Dialog({ title, width, onClose, header, children }: Props) {
  useEffect(() => {
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onClose]);

  const pressedOutside = useRef(false);

  return (
    <div
      className={styles.overlay}
      onPointerDown={(e) => {
        pressedOutside.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        if (pressedOutside.current && e.target === e.currentTarget) onClose();
      }}
    >
      <section role="dialog" aria-modal="true" aria-label={title} className={styles.dialog} style={{ maxWidth: width }}>
        <div className={styles.head}>
          <h2 className={styles.title}>{title}</h2>
          {header}
          <button type="button" className={styles.close} aria-label="Close" onClick={onClose}>
            <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M3 3l10 10M13 3L3 13" />
            </svg>
          </button>
        </div>
        {children}
      </section>
    </div>
  );
}

/** Cancel and the dialog's main action, at its bottom right. */
export function DialogActions({ children }: { children: ReactNode }) {
  return <div className={styles.actions}>{children}</div>;
}
