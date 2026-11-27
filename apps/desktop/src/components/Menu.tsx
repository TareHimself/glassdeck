import type { ButtonHTMLAttributes, HTMLAttributes } from "react";
import { Link, type LinkProps } from "react-router";
import { classes } from "./classes";
import styles from "./Menu.module.css";

export function Menu({ className, ...rest }: HTMLAttributes<HTMLDivElement>) {
  return <div role="menu" className={classes(styles.menu, className)} {...rest} />;
}

export function MenuItem({ danger, className, ...rest }: ButtonHTMLAttributes<HTMLButtonElement> & { danger?: boolean }) {
  return <button type="button" role="menuitem" className={classes(styles.item, className)} data-danger={danger || undefined} {...rest} />;
}

export function MenuLink(props: LinkProps) {
  return <Link role="menuitem" className={styles.item} {...props} />;
}

export function MenuDivider() {
  return <div className={styles.divider} />;
}
