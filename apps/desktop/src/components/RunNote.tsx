import { runNote, type RunStatus } from "../runStatus";
import styles from "./RunNote.module.css";

/** Over a preview: why it isn't showing the latest edit, if it isn't. */
export function RunNote({ status }: { status: RunStatus }) {
  const note = runNote(status);
  return note && (
    <div className={styles.note} role="status" data-state={status.state}>
      {note}
    </div>
  );
}
