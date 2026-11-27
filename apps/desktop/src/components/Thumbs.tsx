import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import type { BackgroundSummary, Value, WidgetSummary } from "../bindings";
import { pngUrl, useLivePackageCanvas, useStill, useStreamSnapshot, useStreamVideo, useVideoSource, useWidgetPreview } from "../previews";
import { classes } from "./classes";
import { PreviewBadge } from "./PreviewBadge";
import styles from "./Thumbs.module.css";

type Values = { [key: string]: Value };

/** What shows while a live view runs: on a card, the previewing badge; elsewhere, a small spinner until its first frame. */
function LiveIndicator({ badge, playing }: { badge: boolean; playing: boolean }) {
  if (badge) return <PreviewBadge playing={playing} />;
  return playing ? null : <span className={styles.spinner} role="status" aria-label="Loading the live preview" />;
}

/** The canvas drawn live in WebGL over its still, shown from its first frame. */
function LiveCanvasLayer({ id, values, badge }: { id: string; values?: Values; badge: boolean }) {
  const { ref, drawing } = useLivePackageCanvas(id, values);
  return (
    <>
      <canvas ref={ref} className={classes(styles.fill, styles.live)} data-hidden={!drawing || undefined} />
      <LiveIndicator badge={badge} playing={drawing} />
    </>
  );
}

type FrameProps = { background: Pick<BackgroundSummary, "id" | "preview">; live: boolean; badge: boolean };

function CanvasFrame({ background, live, values, badge }: FrameProps & { values?: Values }) {
  const still = useStill(background.id, background.preview);
  return (
    <>
      {still && <img className={styles.fill} alt="" src={still} />}
      {live && <LiveCanvasLayer id={background.id} values={values} badge={badge} />}
    </>
  );
}

function VideoFrame({ background, live, badge }: FrameProps) {
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
      {live && <LiveIndicator badge={badge} playing={playing} />}
    </>
  );
}

/**
 * A background as its still, playing over it while `live`; its kind's pattern until it has a still.
 * `badge`: a card shows the previewing badge while it plays, rather than a spinner.
 */
export function BackgroundThumb({
  background,
  live = false,
  badge = false,
  values,
  className,
  children,
}: {
  background: Pick<BackgroundSummary, "id" | "kind" | "preview">;
  live?: boolean;
  badge?: boolean;
  values?: Values;
  className?: string;
  children?: ReactNode;
}) {
  return (
    <div className={classes(styles.background, className)} data-kind={background.kind}>
      {background.kind === "canvas" ? (
        <CanvasFrame background={background} live={live} values={values} badge={badge} />
      ) : (
        <VideoFrame background={background} live={live} badge={badge} />
      )}
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

/** A widget at its size on a sample background, as its still, or running over it while `live` (see `BackgroundThumb` for `badge`). */
export function WidgetThumb({
  widget,
  live = false,
  badge = false,
  className,
}: {
  widget: Pick<WidgetSummary, "id" | "title" | "sizing" | "preview">;
  live?: boolean;
  badge?: boolean;
  className?: string;
}) {
  const { w, h } = widget.sizing.size;
  const png = useWidgetPreview(live ? widget.id : null, widget.sizing.size);
  const still = useStill(widget.id, widget.preview);
  const liveSrc = png && pngUrl(png);
  return (
    <div className={classes(styles.stage, className)}>
      <div className={styles.footprint} data-empty={!still || undefined} style={{ "--w": w, "--h": h } as CSSProperties}>
        {still && <img className={styles.fill} alt={widget.title} src={still} />}
        {live && liveSrc && <img className={classes(styles.fill, styles.live)} alt={widget.title} src={liveSrc} />}
        {live && !badge && <LiveIndicator badge={false} playing={liveSrc !== null} />}
      </div>
      {live && badge && <PreviewBadge playing={liveSrc !== null} />}
    </div>
  );
}
