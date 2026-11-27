import type { ReactNode } from "react";
import { Dialog as RadixDialog } from "radix-ui";
import styles from "./Dialog.module.css";

type Props = {
  title: string;
  width: number;
  onClose: () => void;
  /** Controls beside the title, such as a type switch */
  header?: ReactNode;
  children: ReactNode;
};

/** A dialog over the page, open while it's rendered: Escape, the close button or a click outside it dismisses it. */
export function Dialog({ title, width, onClose, header, children }: Props) {
  return (
    <RadixDialog.Root
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <RadixDialog.Portal>
        <RadixDialog.Overlay className={styles.overlay}>
          <RadixDialog.Content className={styles.dialog} style={{ maxWidth: width }} aria-describedby={undefined}>
            <div className={styles.head}>
              <RadixDialog.Title className={styles.title}>{title}</RadixDialog.Title>
              {header}
              <RadixDialog.Close className={styles.close} aria-label="Close">
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M3 3l10 10M13 3L3 13" />
                </svg>
              </RadixDialog.Close>
            </div>
            {children}
          </RadixDialog.Content>
        </RadixDialog.Overlay>
      </RadixDialog.Portal>
    </RadixDialog.Root>
  );
}

/** Cancel and the dialog's main action, at its bottom right. */
export function DialogActions({ children }: { children: ReactNode }) {
  return <div className={styles.actions}>{children}</div>;
}
