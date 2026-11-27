import type { ReactNode } from "react";
import { ToggleGroup } from "radix-ui";
import styles from "./Segments.module.css";

export function Segments<T extends string>(props: {
  label: string;
  options: { value: T; name: ReactNode }[];
  value: T;
  onChange: (value: T) => void;
  compact?: boolean;
}) {
  return (
    <ToggleGroup.Root
      type="single"
      aria-label={props.label}
      className={styles.segments}
      data-compact={props.compact || undefined}
      value={props.value}
      onValueChange={(value) => {
        const picked = props.options.find((option) => option.value === value);
        if (picked) props.onChange(picked.value);
      }}
    >
      {props.options.map(({ value, name }) => (
        <ToggleGroup.Item key={value} value={value} className={styles.segment}>
          {name}
        </ToggleGroup.Item>
      ))}
    </ToggleGroup.Root>
  );
}
