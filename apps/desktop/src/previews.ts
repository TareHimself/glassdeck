import { convertFileSrc } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { create } from "zustand";
import { CanvasRenderer } from "./canvasRenderer";
import { BUILT_IN_BACKGROUND, commands, events, type CanvasChange, type Dims, type PreviewRun, type SourceFile, type Still, type Value } from "./bindings";

type Values = { [key: string]: Value };

/** Fresh stills by package id, from `StillUpdate`s, newer than what the summaries carried. */
const useFreshStills = create<{ [id: string]: Still }>(() => ({}));
const requested = new Set<string>();
let listeningForStills = false;

function listenForStills() {
  if (listeningForStills) return;
  listeningForStills = true;
  void events.stillUpdate.listen(({ payload }) => {
    if (payload.state === "ready") useFreshStills.setState({ [payload.id]: payload.still });
    else console.warn(`no still for ${payload.id}: ${payload.reason}`);
  });
}

/** Where the webview loads a still from. One already inline as a data URL is used as it is. */
export function stillSrc(still: Still): string {
  if (still.path.startsWith("data:")) return still.path;
  return `${convertFileSrc(still.path)}?v=${still.stamp}`;
}

/** The package's still as an image URL: the freshest one known, asked for once when there's none yet. */
export function useStill(id: string, fromSummary: Still | null | undefined): string | null {
  listenForStills();
  const fresh = useFreshStills((s) => s[id]);
  const still = fresh ?? fromSummary ?? null;
  useEffect(() => {
    if (still || requested.has(id)) return;
    requested.add(id);
    void commands.requestStill(id);
  }, [id, still]);
  return still ? stillSrc(still) : null;
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

/**
 * A `<canvas>` (give it `ref`) drawing whatever `show` gets, live, through the one canvas preview: the
 * shader runs here in WebGL2, and the app's side runs `keyboard.luau` and sends each frame's inputs.
 * `drawing` turns true at the first frame. `onChange` also hears every change (for an editor's log,
 * inputs and clock).
 */
export function useLiveCanvas(onChange?: (change: CanvasChange) => void) {
  const [element, setElement] = useState<HTMLCanvasElement | null>(null);
  const [drawing, setDrawing] = useState(false);
  const listener = useRef(onChange);
  listener.current = onChange;
  const claim = useRef<CanvasClaim | null>(null);
  const wanted = useRef<{ files: SourceFile[]; values: Values } | null>(null);

  useEffect(() => {
    if (!element) return;
    const report = (text: string) => listener.current?.({ state: "log", source: "keyboard", level: "error", text });
    let renderer: CanvasRenderer;
    try {
      renderer = new CanvasRenderer(element);
    } catch (e) {
      report(e instanceof Error ? e.message : String(e));
      return;
    }
    const own = claimCanvas((change) => {
      try {
        if (change.state === "program") renderer.useProgram(change.fragment, change.width, change.height);
        if (change.state === "frame") {
          renderer.frame({ time: change.time ?? 0, delta: change.delta ?? 0, index: change.index, inputs: change.inputs });
          setDrawing(true);
        }
        if (change.state === "cleared") {
          renderer.clear();
          setDrawing(false);
        }
      } catch (e) {
        console.error("drawing a canvas preview:", e);
        report(e instanceof Error ? e.message : String(e));
      }
      listener.current?.(change);
    });
    claim.current = own;
    if (wanted.current) own.show(wanted.current.files, wanted.current.values);
    return () => {
      own.release();
      if (claim.current === own) claim.current = null;
      renderer.dispose();
      setDrawing(false);
    };
  }, [element]);

  const show = useCallback((files: SourceFile[], values: Values) => {
    wanted.current = { files, values };
    claim.current?.show(files, values);
  }, []);

  return { ref: setElement, drawing, show };
}

/** The canvas `id` drawn live into a `<canvas>`, while mounted. */
export function useLivePackageCanvas(id: string, values: Values = {}) {
  const live = useLiveCanvas();
  const valuesKey = JSON.stringify(values);
  const { show } = live;
  useEffect(() => {
    let alive = true;
    void canvasFiles(id).then((files) => {
      if (alive && files.length > 0) show(files, values);
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, valuesKey, show]);
  return live;
}

/** The widget `id` running alone at `size`, as base64 PNG frames, while `id` isn't null. */
export function useWidgetPreview(id: string | null, size: Dims): string | null {
  const [png, setPng] = useState<string | null>(null);
  useEffect(() => {
    setPng(null);
    if (id === null) return;
    const run = nextPreviewRun();
    const listening = events.previewUpdate.listen(({ payload }) => {
      if (payload.run === run && payload.change.state === "frame") setPng(payload.change.png);
    });
    void commands.previewWidget(id, [], size, {}, run);
    return () => {
      void listening.then((unlisten) => unlisten());
      void commands.stopPreview();
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
