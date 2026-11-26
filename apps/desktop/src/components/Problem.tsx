import type { ReactNode } from "react";
import styles from "./Problem.module.css";

export function Problem({ children }: { children: ReactNode }) {
  return (
    <div role="alert" className={styles.problem}>
      {children}
    </div>
  );
}
