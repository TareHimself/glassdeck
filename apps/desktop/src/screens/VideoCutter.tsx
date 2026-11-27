import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useRef, useState, type PointerEvent } from "react";
import { commands, type VideoFacts } from "../bindings";
import styles from "./VideoCutter.module.css";

/** The bindings type floats as nullable; a cut the editor makes always has all three. */
export type Cut = { crop: number; start: number; end: number };
export type Chosen = { path: string; facts: VideoFacts; cut: Cut };

const PANEL_ASPECT = 16 / 9;
const FRAME_SETTLE_MS = 200;
const TRIM_STEP = 0.1;
/** The shortest loop the trim handles allow. */
const MIN_LOOP = 1;
/** Seeking to the loop's very end shows a frame past it in some files, so its frame is shown this much before. */
const END_FRAME_LEAD = 0.05;

function clock(seconds: number): string {
  const whole = Math.floor(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

/** Where the panel-shaped crop sits in the frame, as percentages of it. */
function cropBox(aspect: number, crop: number) {
  if (aspect >= PANEL_ASPECT) {
    const kept = PANEL_ASPECT / aspect;
    return { axis: "x" as const, start: (1 - kept) * crop * 100, size: kept * 100 };
  }
  const kept = aspect / PANEL_ASPECT;
  return { axis: "y" as const, start: (1 - kept) * crop * 100, size: kept * 100 };
}

function PlayIcon({ playing }: { playing: boolean }) {
  return playing ? (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
      <rect x="3" y="2" width="3.5" height="12" rx="1" />
      <rect x="9.5" y="2" width="3.5" height="12" rx="1" />
    </svg>
  ) : (
    <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
      <path d="M4 2.5v11l9-5.5z" />
    </svg>
  );
}

/**
 * The chosen video playing in a loop between the trim handles, with its crop, a crop slider, and the
 * trim bar: its playhead shows where playback is, and a click on it seeks there. A file the app can't
 * play shows the frame at the seek point instead.
 */
export function VideoCutter({ video, onCut }: { video: Chosen; onCut: (cut: Cut) => void }) {
  const { path, facts, cut } = video;
  const player = useRef<HTMLVideoElement>(null);
  const [playable, setPlayable] = useState(true);
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(cut.start);
  const [still, setStill] = useState<string | null>(null);
  const length = facts.length ?? cut.end;
  const aspect = facts.width / facts.height;
  const box = cropBox(aspect, cut.crop);
  const crops = Math.abs(aspect - PANEL_ASPECT) > 0.01;

  useEffect(() => {
    setPlayable(true);
    setPlaying(false);
    setTime(cut.start);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  useEffect(() => {
    if (playable) return;
    const timer = setTimeout(() => {
      void commands.videoFrame(path, time).then((result) => {
        if (result.status === "ok") setStill(result.data);
      });
    }, FRAME_SETTLE_MS);
    return () => clearTimeout(timer);
  }, [playable, path, time]);

  const seek = (seconds: number) => {
    const to = Math.min(Math.max(seconds, 0), length);
    setTime(to);
    if (player.current) player.current.currentTime = to;
  };

  const togglePlay = () => {
    const element = player.current;
    if (!element) return;
    if (element.paused) {
      if (element.currentTime < cut.start || element.currentTime >= cut.end) element.currentTime = cut.start;
      void element.play();
    } else {
      element.pause();
    }
  };

  const keepInLoop = (element: HTMLVideoElement) => {
    if (element.currentTime >= cut.end || element.currentTime < cut.start - TRIM_STEP) element.currentTime = cut.start;
    setTime(element.currentTime);
  };

  const seekOnBar = (e: PointerEvent<HTMLDivElement>) => {
    if ((e.target as HTMLElement).tagName === "INPUT") return;
    const bar = e.currentTarget.getBoundingClientRect();
    seek(((e.clientX - bar.left) / bar.width) * length);
  };

  const setStart = (value: number) => {
    const start = Math.min(value, cut.end - MIN_LOOP);
    onCut({ ...cut, start });
    seek(start);
  };

  const setEnd = (value: number) => {
    const end = Math.max(value, cut.start + MIN_LOOP);
    onCut({ ...cut, end });
    seek(end - END_FRAME_LEAD);
  };

  const along = box.axis === "x" ? { left: `${box.start}%`, width: `${box.size}%`, top: 0, bottom: 0 } : { top: `${box.start}%`, height: `${box.size}%`, left: 0, right: 0 };
  const before = box.axis === "x" ? { left: 0, top: 0, bottom: 0, width: `${box.start}%` } : { top: 0, left: 0, right: 0, height: `${box.start}%` };
  const after = box.axis === "x" ? { right: 0, top: 0, bottom: 0, width: `${100 - box.start - box.size}%` } : { bottom: 0, left: 0, right: 0, height: `${100 - box.start - box.size}%` };

  return (
    <div className={styles.cutter}>
      <div className={styles.stage}>
        <div className={styles.frame} style={aspect >= PANEL_ASPECT ? { width: "100%", aspectRatio: `${aspect}` } : { height: "100%", aspectRatio: `${aspect}` }}>
          {playable ? (
            <video
              ref={player}
              src={convertFileSrc(path)}
              muted
              playsInline
              preload="auto"
              onLoadedMetadata={(e) => (e.currentTarget.currentTime = cut.start)}
              onTimeUpdate={(e) => keepInLoop(e.currentTarget)}
              onSeeked={(e) => setTime(e.currentTarget.currentTime)}
              onPlay={() => setPlaying(true)}
              onPause={() => setPlaying(false)}
              onError={() => setPlayable(false)}
            />
          ) : (
            still && <img alt="" src={`data:image/jpeg;base64,${still}`} />
          )}
          {crops && (
            <>
              <div className={styles.cropped} style={before} />
              <div className={styles.cropped} style={after} />
              <div className={styles.kept} style={along} />
            </>
          )}
        </div>
      </div>

      {crops && (
        <label className={styles.cropRow}>
          Crop
          <input type="range" min={0} max={1} step={0.01} value={cut.crop} onChange={(e) => onCut({ ...cut, crop: Number(e.target.value) })} />
        </label>
      )}

      <div className={styles.timeline}>
        <button type="button" className={styles.play} aria-label={playing ? "Pause" : "Play"} disabled={!playable} onClick={togglePlay}>
          <PlayIcon playing={playing} />
        </button>
        <div className={styles.trim} onPointerDown={seekOnBar}>
          <div className={styles.trimmed} style={{ left: 0, width: `${(cut.start / length) * 100}%` }} />
          <div className={styles.trimmed} style={{ right: 0, width: `${((length - cut.end) / length) * 100}%` }} />
          <div className={styles.loop} style={{ left: `${(cut.start / length) * 100}%`, right: `${((length - cut.end) / length) * 100}%` }} />
          <div className={styles.playhead} style={{ left: `${(time / length) * 100}%` }} />
          <input type="range" aria-label="Loop start" min={0} max={length} step={TRIM_STEP} value={cut.start} onChange={(e) => setStart(Number(e.target.value))} />
          <input type="range" aria-label="Loop end" min={0} max={length} step={TRIM_STEP} value={cut.end} onChange={(e) => setEnd(Number(e.target.value))} />
        </div>
      </div>
      <div className={styles.times}>
        <span>{clock(cut.start)}</span>
        <span>
          {clock(time)} · Loop {clock(cut.end - cut.start)}
        </span>
        <span>{clock(cut.end)}</span>
      </div>
    </div>
  );
}
