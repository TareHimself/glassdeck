import { useEffect, useRef, type CSSProperties, type ReactNode } from "react";
import type { BackgroundSummary, Value, WidgetSummary } from "../bindings";
import { jpegUrl, pngUrl, useCanvasPreview, useThumbnails, useVideoSource, useWidgetPreview } from "../previews";
import { classes } from "./classes";
import styles from "./Thumbs.module.css";

type Values = { [key: string]: Value };

function CanvasFrame({ id, live, values }: { id: string; live: boolean; values?: Values }) {
  const jpeg = useCanvasPreview(live ? id : null, values);
  const still = useThumbnails((s) => s[id]);
  const src = jpeg ? jpegUrl(jpeg) : still;
  return src ? <img className={styles.fill} alt="" src={src} /> : null;
}

function VideoFrame({ id, live }: { id: string; live: boolean }) {
  const source = useVideoSource(id);
  const video = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    const element = video.current;
    if (!element) return;
    if (live) {
      element.play().catch((e: unknown) => {
        if (!(e instanceof DOMException && e.name === "AbortError")) console.error("playing a video thumbnail", e);
      });
    } else {
      element.pause();
      element.currentTime = 0;
    }
  }, [live, source]);
  if (!source) return null;
  return <video ref={video} className={styles.fill} src={source.src} poster={source.poster ?? undefined} muted loop playsInline preload="metadata" />;
}

/** A background as a still, or playing while `live`; its kind's pattern until there's a frame. */
export function BackgroundThumb({
  background,
  live = false,
  values,
  className,
  children,
}: {
  background: Pick<BackgroundSummary, "id" | "kind">;
  live?: boolean;
  values?: Values;
  className?: string;
  children?: ReactNode;
}) {
  return (
    <div className={classes(styles.background, className)} data-kind={background.kind}>
      {background.kind === "canvas" ? <CanvasFrame id={background.id} live={live} values={values} /> : <VideoFrame id={background.id} live={live} />}
      {children}
    </div>
  );
}

/** A widget at its size on a sample background, as a still, or running while `live`. */
export function WidgetThumb({ widget, live = false, className }: { widget: Pick<WidgetSummary, "id" | "title" | "sizing">; live?: boolean; className?: string }) {
  const { w, h } = widget.sizing.size;
  const png = useWidgetPreview(live ? widget.id : null, widget.sizing.size);
  const still = useThumbnails((s) => s[widget.id]);
  const src = png ? pngUrl(png) : still;
  return (
    <div className={classes(styles.stage, className)}>
      <div className={styles.footprint} data-empty={!src || undefined} style={{ "--w": w, "--h": h } as CSSProperties}>
        {src && <img className={styles.fill} alt={widget.title} src={src} />}
      </div>
    </div>
  );
}
