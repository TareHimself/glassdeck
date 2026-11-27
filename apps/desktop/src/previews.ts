import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { create } from "zustand";
import { BUILT_IN_BACKGROUND, commands, events, type CanvasChange, type Dims, type PreviewRun, type SourceFile, type Value } from "./bindings";

type Values = { [key: string]: Value };

/** The last frame seen of each package, by qualified id, as an image URL. Lasts while the app runs. */
export const useThumbnails = create<{ [id: string]: string }>(() => ({}));

export function rememberThumbnail(id: string, url: string) {
  useThumbnails.setState({ [id]: url });
}

export const jpegUrl = (base64: string) => `data:image/jpeg;base64,${base64}`;
export const pngUrl = (base64: string) => `data:image/png;base64,${base64}`;

function canvasFiles(id: string): Promise<SourceFile[]> {
  if (id === BUILT_IN_BACKGROUND) return commands.canvasTemplate();
  return commands.readCanvas(id).then((r) => (r.status === "ok" ? r.data.files : []));
}

let lastRun = 0;

/** A fresh id for one start of a preview; the preview stamps everything that start produces with it. */
export function nextPreviewRun(): PreviewRun {
  lastRun += 1;
  return lastRun;
}

/**
 * There is one canvas preview. Whoever claimed it last owns it and hears only its own runs; when they
 * let go, the claim before theirs starts again. This lets a picker or a dialog preview a canvas over
 * the Overview's background, and an editor take it over entirely.
 */
export type CanvasClaim = {
  /** Draws these files: a new run, started now if this claim owns the preview, or when it next does. */
  show: (files: SourceFile[], values: Values) => void;
  release: () => void;
};

type Claim = { onChange: (change: CanvasChange) => void; run: PreviewRun | null; wanted: { files: SourceFile[]; values: Values } | null };
const claims: Claim[] = [];
let listening: Promise<() => void> | null = null;

function startOwner() {
  const owner = claims.at(-1);
  if (!owner?.wanted) {
    void commands.stopCanvas();
    return;
  }
  listening ??= events.canvasUpdate.listen(({ payload }) => {
    const current = claims.at(-1);
    if (current && payload.run === current.run) current.onChange(payload.change);
  });
  owner.run = nextPreviewRun();
  void commands.previewCanvas(owner.wanted.files, owner.wanted.values, owner.run);
}

export function claimCanvas(onChange: (change: CanvasChange) => void): CanvasClaim {
  const claim: Claim = { onChange, run: null, wanted: null };
  claims.push(claim);
  startOwner();
  return {
    show: (files, values) => {
      claim.wanted = { files, values };
      if (claims.at(-1) === claim) startOwner();
    },
    release: () => {
      const owned = claims.at(-1) === claim;
      claims.splice(claims.indexOf(claim), 1);
      if (owned) startOwner();
    },
  };
}

/** The canvas `id` drawn live, as base64 JPEG frames, while `id` isn't null. */
export function useCanvasPreview(id: string | null, values: Values = {}): string | null {
  const [jpeg, setJpeg] = useState<string | null>(null);
  const valuesKey = JSON.stringify(values);
  useEffect(() => {
    setJpeg(null);
    if (id === null) return;
    let last: string | null = null;
    let alive = true;
    const claim = claimCanvas((change) => {
      if (change.state === "frame") {
        if (last === null) rememberThumbnail(id, jpegUrl(change.jpeg));
        last = change.jpeg;
        setJpeg(change.jpeg);
      } else if (change.state === "cleared") {
        setJpeg(null);
      }
    });
    void canvasFiles(id).then((files) => {
      if (alive && files.length > 0) claim.show(files, values);
    });
    return () => {
      alive = false;
      claim.release();
      if (last) rememberThumbnail(id, jpegUrl(last));
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, valuesKey]);
  return jpeg;
}

/** The widget `id` running alone at `size`, as base64 PNG frames, while `id` isn't null. */
export function useWidgetPreview(id: string | null, size: Dims): string | null {
  const [png, setPng] = useState<string | null>(null);
  useEffect(() => {
    setPng(null);
    if (id === null) return;
    let last: string | null = null;
    const run = nextPreviewRun();
    const listening = events.previewUpdate.listen(({ payload }) => {
      if (payload.run !== run || payload.change.state !== "frame") return;
      if (last === null) rememberThumbnail(id, pngUrl(payload.change.png));
      last = payload.change.png;
      setPng(payload.change.png);
    });
    void commands.previewWidget(id, [], size, {}, run);
    return () => {
      void listening.then((unlisten) => unlisten());
      void commands.stopPreview();
      if (last) rememberThumbnail(id, pngUrl(last));
    };
  }, [id, size.w, size.h]);
  return png;
}

export type VideoSource = { src: string; poster: string | null };

/** The video background `id`'s converted file and still, while `id` isn't null. */
export function useVideoSource(id: string | null): VideoSource | null {
  const [source, setSource] = useState<VideoSource | null>(null);
  useEffect(() => {
    setSource(null);
    if (id === null) return;
    let current = true;
    void commands.readVideo(id).then((r) => {
      const video = r.status === "ok" ? r.data.video : null;
      if (!current || !video) return;
      setSource({ src: convertFileSrc(video.path), poster: video.preview && jpegUrl(video.preview) });
    });
    return () => {
      current = false;
    };
  }, [id]);
  return source;
}
