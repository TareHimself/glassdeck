import { useEffect, useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { commands, type BackgroundKind, type LibraryError, type NewContent, type VideoFacts } from "../bindings";
import { useLiveCanvas } from "../previews";
import { Button } from "../components/Button";
import { Dialog, DialogActions } from "../components/Dialog";
import { Field, Input } from "../components/Input";
import { Loading } from "../components/Loading";
import { Problem } from "../components/Problem";
import { Segments } from "../components/Segments";
import { describeLibraryError, slug } from "../library";
import { chooseLabel, lastChosen, usePicker } from "../picking";
import { CanvasIcon, ImageIcon, KIND_NAMES, VideoIcon } from "./backgroundKinds";
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
  {
    value: "image",
    name: (
      <>
        <ImageIcon />
        {KIND_NAMES.image}
      </>
    ),
  },
];

/** The starting gradient, drawn live while Canvas is chosen, or why it stopped. */
function useStartingCanvas() {
  const [error, setError] = useState<string | null>(null);
  const live = useLiveCanvas((change) => {
    if (change.state === "log" && change.level === "error") setError(change.text);
  });
  const { show } = live;
  useEffect(() => {
    commands
      .canvasTemplate()
      .then((files) => show(files, {}))
      .catch((e: unknown) => setError(String(e)));
  }, [show]);
  return { ...live, error };
}

export function NewBackgroundDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate();
  const [kind, setKind] = useState<BackgroundKind>("canvas");
  const [title, setTitle] = useState("");
  const [video, setVideo] = useState<{ path: string; facts: VideoFacts } | null>(null);
  const [cut, setCut] = useState<Cut | null>(null);
  const pictures = usePicker(commands.pickImage, commands.previewImage);
  const picture = lastChosen(pictures.picking);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);
  const canvasFrame = useStartingCanvas();
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
    if (!title) setTitle(fileTitle(path));
  };

  const pictureFile = picture?.path;
  useEffect(() => {
    if (pictureFile) setTitle((current) => current || fileTitle(pictureFile));
  }, [pictureFile]);

  const content = (): NewContent | null => {
    switch (kind) {
      case "canvas":
        return { kind: "canvas" };
      case "video":
        return video && cut ? { kind: "video", source: video.path, cut } : null;
      case "image":
        return pictures.picking.state === "chosen" ? { kind: "image", source: pictures.picking.chosen.path } : null;
    }
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
            <div className={styles.stage}>
              <canvas ref={canvasFrame.ref} className={styles.canvas} data-hidden={!canvasFrame.drawing || undefined} />
            </div>
          )}
          {kind === "canvas" && canvasFrame.error && <Problem>{canvasFrame.error}</Problem>}
          {kind === "video" && (chosenVideo ? <VideoCutter video={chosenVideo} onCut={setCut} /> : <div className={styles.stage} />)}
          {kind === "image" && (
            <div className={styles.stage}>
              {picture && <img className={styles.canvas} alt="How the picture fills the keyboard" src={`data:image/jpeg;base64,${picture.value}`} />}
              {pictures.picking.state === "loading" && <Loading label="Fitting the picture to the keyboard" />}
            </div>
          )}
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

          {kind === "image" && (
            <div className={styles.file}>
              <span className={styles.label}>Picture</span>
              <div className={styles.fileRow}>
                <span className={styles.path}>{pictures.picking.state === "loading" ? pictures.picking.path : picture?.path}</span>
                <Button className={styles.choose} disabled={pictures.busy} onClick={() => void pictures.choose()}>
                  {chooseLabel(pictures.picking, "Choose")}
                </Button>
              </div>
            </div>
          )}

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
      {kind === "image" && pictures.problem && <Problem>{describeLibraryError(pictures.problem)}</Problem>}

      <DialogActions>
        <Button variant="quiet" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" disabled={creating || content() === null} onClick={() => void create()}>
          {kind === "video" ? "Convert" : "Create"}
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

/** A file's name without its folder or extension, as a starting title. */
function fileTitle(path: string): string {
  return path.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") ?? "";
}

function clock(seconds: number): string {
  const whole = Math.floor(seconds);
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}
