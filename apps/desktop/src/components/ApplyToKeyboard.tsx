import { Link } from "react-router";
import { applyBackground, applyState, useApplied } from "../applying";
import { useGlassdeckd } from "../glassdeckd";
import { useKeyboard } from "../keyboard";
import { Button } from "./Button";
import styles from "./ApplyToKeyboard.module.css";

type Props = {
  /** The background's qualified id */
  id: string;
  /** Edits not saved yet; Apply saves them first */
  unsaved: boolean;
  /** Saves the edits; false when saving failed */
  save: () => Promise<boolean>;
  apply: { apply: Parameters<typeof applyBackground>[1]; applying: boolean };
};

/** A background editor's Apply to keyboard: saves, makes it the keyboard's background and applies the theme. */
export function ApplyToKeyboard({ id, unsaved, save, apply }: Props) {
  const needsSetup = useKeyboard((s) => s.keyboard?.software === "flux");
  const onKeyboard = useGlassdeckd((s) => s.theme?.background.package === id);
  const saves = useApplied((s) => s.saves[id] ?? 0);
  const applied = useApplied((s) => s.applied[id] ?? 0);
  const state = applyState({ needsSetup, onKeyboard, unsaved, saves, applied });

  switch (state) {
    case "set up":
      return (
        <>
          <Link className={styles.setUp} to="/keyboard">
            Set up the keyboard to apply
          </Link>
          <Button variant="primary" disabled>
            Apply to keyboard
          </Button>
        </>
      );
    case "on keyboard":
      return (
        <Button variant="quiet" className={styles.onKeyboard} disabled>
          On the keyboard
        </Button>
      );
    case "ready":
      return (
        <Button
          variant="primary"
          disabled={apply.applying}
          onClick={() => void (async () => (!unsaved || (await save())) && (await applyBackground(id, apply.apply)))()}
        >
          {apply.applying ? "Applying…" : "Apply to keyboard"}
        </Button>
      );
  }
}
