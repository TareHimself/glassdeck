import styles from "./Stepper.module.css";

export function Stepper(props: { value: string; lessLabel: string; moreLabel: string; onLess: () => void; onMore: () => void }) {
  return (
    <div className={styles.stepper}>
      <button type="button" className={styles.step} aria-label={props.lessLabel} onClick={props.onLess}>
        −
      </button>
      <span className={styles.value}>{props.value}</span>
      <button type="button" className={styles.step} aria-label={props.moreLabel} onClick={props.onMore}>
        +
      </button>
    </div>
  );
}
