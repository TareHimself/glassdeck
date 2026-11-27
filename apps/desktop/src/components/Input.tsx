import type { ComponentProps, ReactNode } from "react";
import { classes } from "./classes";
import styles from "./Input.module.css";

export type InputVariant = "plain" | "mono" | "search" | "inline";

export function Input({ variant = "plain", className, ...rest }: ComponentProps<"input"> & { variant?: InputVariant }) {
  return <input className={classes(styles.input, className)} data-variant={variant} {...rest} />;
}

export function Field({ label, htmlFor, children }: { label: string; htmlFor: string; children: ReactNode }) {
  return (
    <div className={styles.field}>
      <label className={styles.label} htmlFor={htmlFor}>
        {label}
      </label>
      {children}
    </div>
  );
}
