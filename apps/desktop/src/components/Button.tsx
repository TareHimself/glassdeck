import type { ButtonHTMLAttributes } from "react";
import { Link, type LinkProps } from "react-router";
import styles from "./Button.module.css";
import { classes } from "./classes";

export type ButtonVariant = "primary" | "secondary" | "quiet";

type Variant = { variant?: ButtonVariant };

export function Button({ variant = "secondary", className, type = "button", ...rest }: ButtonHTMLAttributes<HTMLButtonElement> & Variant) {
  return <button type={type} className={classes(styles.button, className)} data-variant={variant} {...rest} />;
}

export function ButtonLink({ variant = "secondary", className, ...rest }: LinkProps & Variant) {
  return <Link className={classes(styles.button, className)} data-variant={variant} {...rest} />;
}
