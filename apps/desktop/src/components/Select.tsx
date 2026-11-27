import type { ReactNode } from "react";
import { Select as RadixSelect } from "radix-ui";
import { classes } from "./classes";
import styles from "./Select.module.css";

export type SelectOption = { value: string; label: ReactNode };

/** Radix reserves "" for "nothing chosen", so every value is prefixed to let "" be a real choice. */
const toItem = (value: string) => `v:${value}`;
const fromItem = (item: string) => item.slice(2);

export function Select({
  value,
  options,
  onChange,
  label,
  id,
  mono = false,
  className,
}: {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  label?: string;
  id?: string;
  mono?: boolean;
  className?: string;
}) {
  return (
    <RadixSelect.Root value={toItem(value)} onValueChange={(item) => onChange(fromItem(item))}>
      <RadixSelect.Trigger id={id} aria-label={label} className={classes(styles.trigger, className)} data-mono={mono || undefined}>
        <RadixSelect.Value />
        <RadixSelect.Icon className={styles.icon}>
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="1.8">
            <path d="M2.5 4.5L6 8l3.5-3.5" />
          </svg>
        </RadixSelect.Icon>
      </RadixSelect.Trigger>
      <RadixSelect.Portal>
        <RadixSelect.Content className={styles.content} position="popper" sideOffset={6} data-mono={mono || undefined}>
          <RadixSelect.Viewport>
            {options.map((option) => (
              <RadixSelect.Item key={option.value} value={toItem(option.value)} className={styles.item}>
                <RadixSelect.ItemText>{option.label}</RadixSelect.ItemText>
              </RadixSelect.Item>
            ))}
          </RadixSelect.Viewport>
        </RadixSelect.Content>
      </RadixSelect.Portal>
    </RadixSelect.Root>
  );
}
