import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { create } from "zustand";
import { BUILT_IN_BACKGROUND, commands, events, type Dims, type SourceFile, type Value } from "./bindings";

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

/**
 * There is one canvas preview. Whoever claimed it last gets its frames; when they let go, the claim
 * before theirs starts again. This lets a picker preview a canvas over the Overview's background.
 */
type Claim = { id: string; values: Values; show: (jpeg: string | null) => void; last: string | null };
const claims: Claim[] = [];
let listening: Promise<() => void> | null = null;

function startTopClaim() {
  const top = claims.at(-1);
  if (!top) {
    void commands.stopCanvas();
    return;
  }
  listening ??= events.canvasUpdate.listen(({ payload }) => {
    const owner = claims.at(-1);
    if (!owner) return;
    if (payload.state === "frame") {
      if (owner.last === null) rememberThumbnail(owner.id, jpegUrl(payload.jpeg));
      owner.last = payload.jpeg;
      owner.show(payload.jpeg);
    } else if (payload.state === "cleared") {
      owner.show(null);
    }
  });
  top.last = null;
  void canvasFiles(top.id).then((files) => {
    if (claims.at(-1) === top && files.length > 0) void commands.previewCanvas(files, top.values);
  });
}

function release(claim: Claim) {
  const wasTop = claims.at(-1) === claim;
  claims.splice(claims.indexOf(claim), 1);
  if (claim.last) rememberThumbnail(claim.id, jpegUrl(claim.last));
  if (wasTop) startTopClaim();
}

/** The canvas `id` drawn live, as base64 JPEG frames, while `id` isn't null. */
export function useCanvasPreview(id: string | null, values: Values = {}): string | null {
  const [jpeg, setJpeg] = useState<string | null>(null);
  const valuesKey = JSON.stringify(values);
  useEffect(() => {
    setJpeg(null);
    if (id === null) return;
    const claim: Claim = { id, values, show: setJpeg, last: null };
    claims.push(claim);
    startTopClaim();
    return () => release(claim);
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
    const listening = events.previewUpdate.listen(({ payload }) => {
      if (payload.state !== "frame") return;
      if (last === null) rememberThumbnail(id, pngUrl(payload.png));
      last = payload.png;
      setPng(payload.png);
    });
    void commands.previewWidget(id, [], size, {});
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
