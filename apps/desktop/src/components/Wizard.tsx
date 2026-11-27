import type { ReactNode } from "react";
import { classes } from "./classes";
import styles from "./Wizard.module.css";

/** The numbered step pills at the top of a New widget or New background wizard. */
export function WizardSteps(props: { steps: readonly string[]; step: number; onStep: (step: number) => void }) {
  return (
    <ol aria-label="Steps" className={styles.steps}>
      {props.steps.map((name, i) => (
        <li key={name}>
          <button type="button" className={styles.step} aria-current={i === props.step ? "step" : undefined} onClick={() => props.onStep(i)}>
            <span className={styles.number} data-reached={i <= props.step || undefined}>
              {i + 1}
            </span>
            {name}
          </button>
        </li>
      ))}
    </ol>
  );
}

export function WizardForm(props: { className?: string; children: ReactNode }) {
  return <section className={classes(styles.form, props.className)}>{props.children}</section>;
}

/** Back or Cancel on the left, Next or the final action on the right. */
export function WizardActions(props: { children: ReactNode }) {
  return <div className={styles.actions}>{props.children}</div>;
}
