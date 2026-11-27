import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import type { BackgroundSummary, Value, WidgetSummary } from "../bindings";
import { pngUrl, useLivePackageCanvas, useStill, useStreamSnapshot, useStreamVideo, useVideoSource, useWidgetPreview } from "../previews";
import { classes } from "./classes";
import styles from "./Thumbs.module.css";

type Values = { [key: string]: Value };

/**
 * The still, with the live view over it once its first frame is in: it fades in, and a small spinner
 * shows while it's starting. Never another package's picture in between.
 */
function Layers({ still, live, liveSrc, alt }: { still: string | null; live: boolean; liveSrc: string | null; alt: string }) {
  return (
    <>
      {still && <img className={styles.fill} alt={alt} src={still} />}
      {live && liveSrc && <img className={classes(styles.fill, styles.live)} alt={alt} src={liveSrc} />}
      {live && !liveSrc && <span className={styles.spinner} role="status" aria-label="Loading the live preview" />}
    </>
  );
}

/** The canvas drawn live in WebGL over its still, shown from its first frame. */
function LiveCanvasLayer({ id, values }: { id: string; values?: Values }) {
  const { ref, drawing } = useLivePackageCanvas(id, values);
  return (
    <>
      <canvas ref={ref} className={classes(styles.fill, styles.live)} data-hidden={!drawing || undefined} />
      {!drawing && <span className={styles.spinner} role="status" aria-label="Loading the live preview" />}
    </>
  );
}

function CanvasFrame({ background, live, values }: { background: Pick<BackgroundSummary, "id" | "preview">; live: boolean; values?: Values }) {
  const still = useStill(background.id, background.preview);
  return (
    <>
      {still && <img className={styles.fill} alt="" src={still} />}
      {live && <LiveCanvasLayer id={background.id} values={values} />}
    </>
  );
}

function VideoFrame({ background, live }: { background: Pick<BackgroundSummary, "id" | "preview">; live: boolean }) {
  const source = useVideoSource(background.id);
  const still = useStill(background.id, background.preview);
  const video = useRef<HTMLVideoElement>(null);
  const [playing, setPlaying] = useState(false);
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
      setPlaying(false);
    }
  }, [live, source]);
  return (
    <>
      {still && <img className={styles.fill} alt="" src={still} />}
      {source && (
        <video
          ref={video}
          className={classes(styles.fill, styles.live)}
          data-hidden={!(live && playing) || undefined}
          src={source.src}
          muted
          loop
          playsInline
          preload="metadata"
          onPlaying={() => setPlaying(true)}
        />
      )}
      {live && !playing && <span className={styles.spinner} role="status" aria-label="Loading the live preview" />}
    </>
  );
}

/** A background as its still, playing over it while `live`; its kind's pattern until it has a still. */
export function BackgroundThumb({
  background,
  live = false,
  values,
  className,
  children,
}: {
  background: Pick<BackgroundSummary, "id" | "kind" | "preview">;
  live?: boolean;
  values?: Values;
  className?: string;
  children?: ReactNode;
}) {
  return (
    <div className={classes(styles.background, className)} data-kind={background.kind}>
      {background.kind === "canvas" ? <CanvasFrame background={background} live={live} values={values} /> : <VideoFrame background={background} live={live} />}
      {children}
    </div>
  );
}

/** A stream playing live, over snapshots of it until the first frame decodes or whenever it drops. */
export function StreamLayer({ name, className }: { name: string; className?: string }) {
  const { ref, playing } = useStreamVideo(name);
  const snapshot = useStreamSnapshot(name, !playing);
  return (
    <>
      {snapshot && <img className={className} alt={`${name} now`} src={snapshot} />}
      <video ref={ref} className={classes(className, styles.live)} data-hidden={!playing || undefined} aria-label={`${name} live`} muted autoPlay playsInline />
    </>
  );
}

/** A widget at its size on a sample background, as its still, or running over it while `live`. */
export function WidgetThumb({ widget, live = false, className }: { widget: Pick<WidgetSummary, "id" | "title" | "sizing" | "preview">; live?: boolean; className?: string }) {
  const { w, h } = widget.sizing.size;
  const png = useWidgetPreview(live ? widget.id : null, widget.sizing.size);
  const still = useStill(widget.id, widget.preview);
  return (
    <div className={classes(styles.stage, className)}>
      <div className={styles.footprint} data-empty={!still || undefined} style={{ "--w": w, "--h": h } as CSSProperties}>
        <Layers still={still} live={live} liveSrc={png && pngUrl(png)} alt={widget.title} />
      </div>
    </div>
  );
}
