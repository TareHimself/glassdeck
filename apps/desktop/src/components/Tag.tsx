import type { HTMLAttributes } from "react";
import { classes } from "./classes";
import styles from "./Tag.module.css";

export type TagTone = "plain" | "host" | "active";

export function Tag({ tone = "plain", className, ...rest }: HTMLAttributes<HTMLSpanElement> & { tone?: TagTone }) {
  return <span className={classes(styles.tag, className)} data-tone={tone} {...rest} />;
}
