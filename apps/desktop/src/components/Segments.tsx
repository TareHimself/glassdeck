import styles from "./Segments.module.css";

export function Segments<T extends string>(props: {
  label: string;
  options: { value: T; name: string }[];
  value: T;
  onChange: (value: T) => void;
  compact?: boolean;
}) {
  return (
    <div role="group" aria-label={props.label} className={styles.segments} data-compact={props.compact || undefined}>
      {props.options.map(({ value, name }) => (
        <button key={value} type="button" className={styles.segment} aria-pressed={value === props.value} onClick={() => props.onChange(value)}>
          {name}
        </button>
      ))}
    </div>
  );
}
