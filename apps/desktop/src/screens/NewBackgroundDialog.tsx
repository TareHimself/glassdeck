import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { commands, events, type BackgroundKind, type LibraryError, type NewContent, type VideoFacts } from "../bindings";
import { Button } from "../components/Button";
import { Dialog, DialogActions } from "../components/Dialog";
import { Field, Input } from "../components/Input";
import { Problem } from "../components/Problem";
import { Segments } from "../components/Segments";
import { describeLibraryError, slug } from "../library";
import { CanvasIcon, KIND_NAMES, VideoIcon } from "./backgroundKinds";
import { VideoCutter, type Chosen, type Cut } from "./VideoCutter";
import styles from "./NewBackgroundDialog.module.css";

const ID_PREFIX = "local.user";

const KINDS: { value: BackgroundKind; name: ReactNode }[] = [
  {
    value: "canvas",
    name: (
      <>
        <CanvasIcon />
        {KIND_NAMES.canvas}
      </>
    ),
  },
  {
    value: "video",
    name: (
      <>
        <VideoIcon />
        {KIND_NAMES.video}
      </>
    ),
  },
];

/** The starting gradient, drawn live by the canvas preview while Canvas is chosen; its frame, or why it stopped. */
function useCanvasPreview(running: boolean): { jpeg: string | null; error: string | null } {
  const [jpeg, setJpeg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!running) return;
    const listening = events.canvasUpdate.listen(({ payload }) => {
      if (payload.state === "frame") setJpeg(payload.jpeg);
      if (payload.state === "log" && payload.level === "error") setError(payload.text);
    });
    commands
      .canvasTemplate()
      .then((files) => commands.previewCanvas(files, {}))
      .catch((e: unknown) => setError(String(e)));
    return () => {
      void listening.then((unlisten) => unlisten());
      void commands.stopCanvas();
    };
  }, [running]);
  return { jpeg, error };
}

export function NewBackgroundDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate();
  const [kind, setKind] = useState<BackgroundKind>("canvas");
  const [title, setTitle] = useState("");
  const [video, setVideo] = useState<{ path: string; facts: VideoFacts } | null>(null);
  const [cut, setCut] = useState<Cut | null>(null);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);
  const canvasFrame = useCanvasPreview(kind === "canvas");
  const id = `${ID_PREFIX}.${slug(title) || "background"}`;

  const chooseVideo = async () => {
    const path = await commands.pickVideo();
    if (!path) return;
    const facts = await commands.videoFacts(path);
    if (facts.status === "error") {
      setProblem(facts.error);
      return;
    }
    setProblem(null);
    setVideo({ path, facts: facts.data });
    setCut(facts.data.length === null ? null : { crop: 0.5, start: 0, end: facts.data.length });
    if (!title) setTitle(path.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") ?? "");
  };

  const content = (): NewContent | null => {
    if (kind === "canvas") return { kind: "canvas" };
    return video && cut ? { kind: "video", source: video.path, cut } : null;
  };

  const create = async () => {
    const chosen = content();
    if (!chosen) return;
    setCreating(true);
    const result = await commands.createBackground({ title, id, content: chosen });
    setCreating(false);
    if (result.status === "ok") navigate(`/backgrounds/${result.data.id}`);
    else setProblem(result.error);
  };

  const chosenVideo: Chosen | null = video && cut ? { ...video, cut } : null;

  return (
    <Dialog title="New background" width={880} onClose={onClose} header={<Segments compact label="Type" options={KINDS} value={kind} onChange={setKind} />}>
      <div className={styles.body}>
        <div className={styles.preview}>
          {kind === "canvas" && (
            <div className={styles.stage}>{canvasFrame.jpeg && <img className={styles.canvas} alt="" src={`data:image/jpeg;base64,${canvasFrame.jpeg}`} />}</div>
          )}
          {kind === "canvas" && canvasFrame.error && <Problem>{canvasFrame.error}</Problem>}
          {kind === "video" && (chosenVideo ? <VideoCutter video={chosenVideo} onCut={setCut} /> : <div className={styles.stage} />)}
        </div>

        <div className={styles.form}>
          <Field label="Title" htmlFor="nb-title">
            <Input
              id="nb-title"
              type="text"
              value={title}
              autoFocus
              onChange={(e) => {
                setTitle(e.target.value);
                setProblem(null);
              }}
            />
            <span className={styles.id}>background.{id}</span>
          </Field>

          {kind === "video" && (
            <div className={styles.file}>
              <span className={styles.label}>Video file</span>
              <div className={styles.fileRow}>
                <span className={styles.path}>{video?.path}</span>
                <Button className={styles.choose} onClick={() => void chooseVideo()}>
                  Choose
                </Button>
              </div>
              {video && (
                <>
                  <div className={styles.facts}>
                    <span>{video.facts.codec}</span>
                    <span>
                      {video.facts.width} × {video.facts.height}
                    </span>
                    <span>{Math.round(video.facts.fps ?? 0)} fps</span>
                    {video.facts.length !== null && <span>{clock(video.facts.length)}</span>}
                  </div>
                  {cut && reencodes(video.facts, cut) && <span className={styles.note}>Cropped and re-encoded on create</span>}
                </>
              )}
            </div>
          )}
        </div>
      </div>

      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <DialogActions>
        <Button variant="quiet" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" disabled={creating || content() === null} onClick={() => void create()}>
          {kind === "canvas" ? "Create" : "Convert"}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

/** A file already in the panel's spec, kept whole, is copied as it is. */
function reencodes(facts: VideoFacts, cut: Cut): boolean {
  const whole = cut.start <= 0 && facts.length !== null && cut.end >= facts.length;
  return !(facts.fits_panel && whole);
}

function clock(seconds: number): string {
  const whole = Math.floor(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}
