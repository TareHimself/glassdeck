import { useEffect, useState } from "react";
import { commands } from "../bindings";
import type { FileDrag } from "../fileDrop";
import { KIND_NAMES } from "../screens/backgroundKinds";
import thumbs from "./Thumbs.module.css";
import styles from "./DropOverlay.module.css";

/** The picture fitted to the keyboard, once read; null while it loads or for a video. */
function usePicturePreview(path: string | null): string | null {
  const [shown, setShown] = useState<{ path: string; jpeg: string } | null>(null);
  useEffect(() => {
    if (!path) return;
    let alive = true;
    void commands.previewImage(path).then((result) => {
      if (alive && result.status === "ok") setShown({ path, jpeg: result.data });
    });
    return () => {
      alive = false;
    };
  }, [path]);
  return shown && shown.path === path ? shown.jpeg : null;
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** Over the whole window while files are dragged in: what dropping them will make, or why it won't. */
export function DropOverlay({ drag }: { drag: FileDrag }) {
  const file = drag.state === "one" ? drag.file : null;
  const picture = usePicturePreview(file?.kind === "image" ? file.path : null);
  if (drag.state === "none") return null;
  return (
    <div className={styles.overlay} aria-live="polite">
      <div className={styles.zone} data-refused={drag.state === "refused" || undefined}>
        {file && (
          <div className={styles.card}>
            <div className={`${thumbs.background} ${styles.thumb}`} data-kind={file.kind}>
              {picture && <img className={thumbs.fill} alt="" src={`data:image/jpeg;base64,${picture}`} />}
              <span className={styles.kind} data-kind={file.kind}>
                {KIND_NAMES[file.kind]}
              </span>
            </div>
            <div className={styles.file}>{fileName(file.path)}</div>
          </div>
        )}
        <div className={styles.label}>{file ? `Drop to add ${file.kind === "image" ? "an image" : "a video"} background` : "One image or video at a time"}</div>
      </div>
    </div>
  );
}
