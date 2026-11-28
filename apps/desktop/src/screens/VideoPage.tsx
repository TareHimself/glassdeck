import { noteSaved, useApplyTheme } from "../applying";
import { ApplyToKeyboard } from "../components/ApplyToKeyboard";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { commands, events, type LibraryError, type VideoBackground, type VideoFacts } from "../bindings";
import { Button } from "../components/Button";
import { Dialog, DialogActions } from "../components/Dialog";
import { PageHeader, useBack } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";
import { VideoCutter, type Chosen } from "./VideoCutter";
import styles from "./VideoPage.module.css";

const LIBRARY = { to: "/backgrounds", label: "Backgrounds" };

function clock(seconds: number): string {
  const whole = Math.floor(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

function ReplaceDialog({ id, onClose }: { id: string; onClose: () => void }) {
  const [video, setVideo] = useState<Chosen | null>(null);
  const [problem, setProblem] = useState<LibraryError | null>(null);

  const choose = async () => {
    const path = await commands.pickVideo();
    if (!path) return;
    const facts = await commands.videoFacts(path);
    if (facts.status === "error") return setProblem(facts.error);
    if (facts.data.length === null) return setProblem({ kind: "bad_video", reason: "The video's length can't be read" });
    setProblem(null);
    setVideo({ path, facts: facts.data, cut: { crop: 0.5, start: 0, end: facts.data.length } });
  };

  const convert = async () => {
    if (!video) return;
    const result = await commands.replaceVideo(id, video.path, video.cut);
    if (result.status === "error") setProblem(result.error);
    else onClose();
  };

  return (
    <Dialog title="Replace video" width={720} onClose={onClose}>
      <div className={styles.fileRow}>
        <span className={styles.path}>{video?.path}</span>
        <Button className={styles.small} onClick={() => void choose()}>
          Choose
        </Button>
      </div>
      {video && <VideoCutter video={video} onCut={(cut) => setVideo({ ...video, cut })} />}
      {problem && <Problem>{describeLibraryError(problem)}</Problem>}
      <DialogActions>
        <Button variant="quiet" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" disabled={!video} onClick={() => void convert()}>
          Convert
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function Facts({ facts, from }: { facts: VideoFacts; from: string }) {
  return (
    <div className={styles.facts}>
      <span className={styles.muted}>Codec</span>
      <span>{facts.codec}</span>
      <span className={styles.muted}>Size</span>
      <span>
        {facts.width} × {facts.height}
      </span>
      <span className={styles.muted}>Frame rate</span>
      <span>{Math.round(facts.fps ?? 0)} fps</span>
      {facts.length !== null && (
        <>
          <span className={styles.muted}>Length</span>
          <span>{clock(facts.length)}</span>
        </>
      )}
      <span className={styles.muted}>From</span>
      <span className={styles.from}>{from}</span>
    </div>
  );
}

export function VideoPage({ id }: { id: string }) {
  const back = useBack(LIBRARY);
  const [loaded, setLoaded] = useState<VideoBackground | null>(null);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [playing, setPlaying] = useState(true);
  const [time, setTime] = useState(0);
  const [unplayable, setUnplayable] = useState(false);
  const [staged, setStaged] = useState<{ seconds: number; jpeg: string } | null>(null);
  const [replacing, setReplacing] = useState(false);
  const player = useRef<HTMLVideoElement>(null);

  const reload = async () => {
    const result = await commands.readVideo(id);
    if (result.status === "ok") setLoaded(result.data);
    else setProblem(result.error);
  };

  useEffect(() => {
    void reload();
    const listening = events.videoUpdate.listen(({ payload }) => {
      if (payload.id !== id) return;
      if (payload.state === null) void reload();
      else setLoaded((l) => (l ? { ...l, conversion: payload.state, video: null } : l));
    });
    return () => void listening.then((unlisten) => unlisten());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const applying = useApplyTheme();
  if (!loaded) return problem ? <Problem>{describeLibraryError(problem)}</Problem> : <PageHeader back={back} title="" />;

  const { background, record, video, conversion } = loaded;
  const length = video?.facts.length ?? 0;
  const converting = conversion?.state === "converting" ? conversion.fraction ?? 0 : null;

  const togglePlay = () => {
    const element = player.current;
    if (!element) return;
    if (element.paused) void element.play();
    else element.pause();
  };

  const grabFrame = async () => {
    if (!video) return;
    const result = await commands.videoFrame(video.path, time);
    if (result.status === "ok") setStaged({ seconds: time, jpeg: result.data });
    else setProblem(result.error);
  };

  const save = async (): Promise<boolean> => {
    if (!staged) return true;
    const result = await commands.setVideoPreview(id, staged.seconds);
    if (result.status === "error") {
      setProblem(result.error);
      return false;
    }
    setLoaded({ ...loaded, video: video && { ...video, preview: result.data } });
    setStaged(null);
    noteSaved(id);
    return true;
  };

  const preview = staged?.jpeg ?? video?.preview ?? null;

  return (
    <div className={styles.page}>
      <PageHeader
        back={back}
        title={background.title}
        id={`${background.id} ${background.version}, video background`}
        actions={
          <>
            <Button onClick={() => void save()}>{staged ? "Save" : "Saved"}</Button>
            <ApplyToKeyboard id={id} unsaved={!!staged} save={save} apply={applying} />
          </>
        }
      />

      {problem && <Problem>{describeLibraryError(problem)}</Problem>}
      {applying.problem && <Problem>{applying.problem}</Problem>}
      {conversion?.state === "failed" && <Problem>{conversion.reason}</Problem>}

      <div className={styles.columns}>
        <section aria-label="Preview" className={styles.preview}>
          <div className={styles.stage}>
            {video && !unplayable && (
              <video
                ref={player}
                className={styles.video}
                src={convertFileSrc(video.path)}
                autoPlay
                loop
                muted
                onPlay={() => setPlaying(true)}
                onPause={() => setPlaying(false)}
                onTimeUpdate={(e) => setTime(e.currentTarget.currentTime)}
                onError={() => setUnplayable(true)}
              />
            )}
            {video && unplayable && preview && <img className={styles.video} alt="" src={`data:image/jpeg;base64,${preview}`} />}
            {converting !== null && (
              <div className={styles.converting}>
                <span className={styles.convertingTitle}>Converting</span>
                <div className={styles.bar}>
                  <div className={styles.barFill} style={{ width: `${converting * 100}%` }} />
                </div>
                <span className={styles.percent}>{Math.round(converting * 100)}%</span>
              </div>
            )}
          </div>
          <div className={styles.controls}>
            <button type="button" className={styles.play} aria-label={playing ? "Pause" : "Play"} disabled={!video || unplayable} onClick={togglePlay}>
              {playing ? (
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
                  <rect x="3" y="2" width="3.5" height="12" rx="1" />
                  <rect x="9.5" y="2" width="3.5" height="12" rx="1" />
                </svg>
              ) : (
                <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true" fill="currentColor">
                  <path d="M4 2.5v11l9-5.5z" />
                </svg>
              )}
            </button>
            <div className={styles.progress}>
              <div className={styles.progressFill} style={{ width: length ? `${(time / length) * 100}%` : 0 }} />
            </div>
            <span className={styles.clock}>
              {clock(time)} / {clock(length)}
            </span>
          </div>
          {unplayable && <span className={styles.muted}>This video can't play in the app</span>}
        </section>

        <aside className={styles.aside}>
          <section className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Video</h2>
              <Button className={styles.small} onClick={() => setReplacing(true)}>
                Replace file
              </Button>
            </div>
            {video ? <Facts facts={video.facts} from={record.from} /> : <span className={styles.from}>{record.from}</span>}
          </section>
          <section className={styles.card}>
            <h2 className={styles.cardTitle}>Preview image</h2>
            <div className={styles.still}>
              <div className={styles.thumb}>{preview && <img alt="" src={`data:image/jpeg;base64,${preview}`} />}</div>
              <Button disabled={!video || unplayable} onClick={() => void grabFrame()}>
                Use this frame
              </Button>
            </div>
          </section>
        </aside>
      </div>

      {replacing && (
        <ReplaceDialog
          id={id}
          onClose={() => {
            setReplacing(false);
            setUnplayable(false);
            void reload();
          }}
        />
      )}
    </div>
  );
}
