import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useRef, useState } from "react";
import { IMAGE_EXTENSIONS, VIDEO_EXTENSIONS } from "./bindings";

/** A file a background can be made from. */
export type DroppedFile = { kind: "image" | "video"; path: string };

/** What's being dragged over the window now: one usable file, or something that won't be taken. */
export type FileDrag = { state: "none" } | { state: "one"; file: DroppedFile } | { state: "refused" };

/** One image or one video, by its extension; anything else, or several files, is null. */
export function droppedFile(paths: string[]): DroppedFile | null {
  const [path] = paths;
  if (paths.length !== 1 || path === undefined) return null;
  const extension = /\.([^.\\/]+)$/.exec(path)?.[1]?.toLowerCase() ?? "";
  if ((IMAGE_EXTENSIONS as readonly string[]).includes(extension)) return { kind: "image", path };
  if ((VIDEO_EXTENSIONS as readonly string[]).includes(extension)) return { kind: "video", path };
  return null;
}

/** Follows files dragged over the window; `onDrop` gets one dropped image or video. */
export function useFileDrop(onDrop: (file: DroppedFile) => void): FileDrag {
  const [drag, setDrag] = useState<FileDrag>({ state: "none" });
  const latest = useRef(onDrop);
  latest.current = onDrop;

  useEffect(() => {
    let alive = true;
    let stop: (() => void) | null = null;
    void getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        switch (payload.type) {
          case "enter": {
            const file = droppedFile(payload.paths);
            setDrag(file ? { state: "one", file } : { state: "refused" });
            break;
          }
          case "over":
            break;
          case "leave":
            setDrag({ state: "none" });
            break;
          case "drop": {
            setDrag({ state: "none" });
            const file = droppedFile(payload.paths);
            if (file) latest.current(file);
            break;
          }
        }
      })
      .then((unlisten) => {
        if (alive) stop = unlisten;
        else unlisten();
      });
    return () => {
      alive = false;
      stop?.();
    };
  }, []);

  return drag;
}
