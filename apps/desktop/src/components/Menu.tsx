import type { ReactElement, ReactNode } from "react";
import { DropdownMenu } from "radix-ui";
import { Link, type LinkProps } from "react-router";
import { classes } from "./classes";
import styles from "./Menu.module.css";

/** A dropdown opened by `trigger`; arrow keys, Escape and clicking outside work as expected. */
export function Menu({
  trigger,
  className,
  onCloseAutoFocus,
  children,
}: {
  trigger: ReactElement;
  className?: string;
  /** Call `preventDefault` to leave focus where an item put it instead of returning it to the trigger */
  onCloseAutoFocus?: (e: Event) => void;
  children: ReactNode;
}) {
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>{trigger}</DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className={classes(styles.menu, className)}
          align="end"
          sideOffset={6}
          onCloseAutoFocus={onCloseAutoFocus}
        >
          {children}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

export function MenuItem({ danger, disabled, onSelect, children }: { danger?: boolean; disabled?: boolean; onSelect?: () => void; children: ReactNode }) {
  return (
    <DropdownMenu.Item className={styles.item} data-danger={danger || undefined} disabled={disabled} onSelect={onSelect}>
      {children}
    </DropdownMenu.Item>
  );
}

export function MenuLink(props: LinkProps) {
  return (
    <DropdownMenu.Item asChild className={styles.item}>
      <Link {...props} />
    </DropdownMenu.Item>
  );
}

export function MenuDivider() {
  return <DropdownMenu.Separator className={styles.divider} />;
}
