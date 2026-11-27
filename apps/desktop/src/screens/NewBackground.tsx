import { useState } from "react";
import { useNavigate } from "react-router";
import { commands, type BackgroundKind, type LibraryError, type NewContent } from "../bindings";
import { Button, ButtonLink } from "../components/Button";
import { Field, Input } from "../components/Input";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { WizardActions, WizardForm, WizardSteps } from "../components/Wizard";
import { describeLibraryError, slug } from "../library";
import { CanvasIcon, KIND_NAMES, VideoIcon } from "./backgroundKinds";
import styles from "./NewBackground.module.css";

const ID_PREFIX = "local.me";
const STEPS = ["Type", "Details"] as const;
const KINDS: BackgroundKind[] = ["canvas", "video"];

type Draft = { kind: BackgroundKind; title: string; video: string | null };

function idOf(draft: Draft): string {
  return `${ID_PREFIX}.${slug(draft.title) || "background"}`;
}

/** The step that shows the field a create error is about. */
function stepOf(error: LibraryError): number {
  return error.kind === "invalid_title" || error.kind === "invalid_id" || error.kind === "already_exists" || error.kind === "bad_video" ? 1 : 0;
}

export function NewBackground() {
  const navigate = useNavigate();
  const [step, setStep] = useState(0);
  const [draft, setDraft] = useState<Draft>({ kind: "canvas", title: "", video: null });
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);

  const update = (change: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...change }));
    setProblem(null);
  };

  const chooseVideo = async () => {
    const picked = await commands.pickVideo();
    if (picked) update({ video: picked });
  };

  const content = (): NewContent | null => {
    if (draft.kind === "canvas") return { kind: "canvas" };
    return draft.video ? { kind: "video", source: draft.video } : null;
  };

  const create = async () => {
    const chosen = content();
    if (!chosen) {
      setProblem({ kind: "bad_video", reason: "Choose a video file" });
      return;
    }
    setCreating(true);
    const result = await commands.createBackground({ title: draft.title, id: idOf(draft), content: chosen });
    setCreating(false);
    if (result.status === "ok") {
      navigate(`/backgrounds/${result.data.id}`);
    } else {
      setProblem(result.error);
      setStep(stepOf(result.error));
    }
  };

  return (
    <div className={styles.page}>
      <PageHeader back={{ to: "/backgrounds", label: "Backgrounds" }} title="New background" />

      <WizardSteps steps={STEPS} step={step} onStep={setStep} />

      <WizardForm className={styles.form}>
        {step === 0 && (
          <div className={styles.types}>
            {KINDS.map((kind) => (
              <button key={kind} type="button" className={styles.type} aria-pressed={draft.kind === kind} onClick={() => update({ kind })}>
                <span className={styles.typeThumb} data-kind={kind} />
                <span className={styles.typeName}>
                  {kind === "canvas" ? <CanvasIcon /> : <VideoIcon />}
                  {KIND_NAMES[kind]}
                </span>
              </button>
            ))}
          </div>
        )}

        {step === 1 && (
          <>
            <Field label="Title" htmlFor="bw-title">
              <Input id="bw-title" type="text" value={draft.title} autoFocus onChange={(e) => update({ title: e.target.value })} />
            </Field>
            <Field label="Package id" htmlFor="bw-id">
              <Input id="bw-id" type="text" variant="mono" value={`background.${idOf(draft)}`} readOnly />
            </Field>
            {draft.kind === "video" && (
              <div className={styles.videoField}>
                <span className={styles.label}>Video file</span>
                <div className={styles.video}>
                  <span className={styles.videoPath}>{draft.video}</span>
                  <Button className={styles.choose} onClick={() => void chooseVideo()}>
                    Choose
                  </Button>
                </div>
              </div>
            )}
          </>
        )}

        {problem && <Problem>{describeLibraryError(problem)}</Problem>}

        <WizardActions>
          {step === 0 ? (
            <ButtonLink variant="quiet" to="/backgrounds">
              Cancel
            </ButtonLink>
          ) : (
            <Button variant="quiet" onClick={() => setStep(0)}>
              Back
            </Button>
          )}
          {step === 0 ? (
            <Button variant="primary" onClick={() => setStep(1)}>
              Next
            </Button>
          ) : (
            <Button variant="primary" disabled={creating} onClick={() => void create()}>
              {draft.kind === "canvas" ? "Create and edit" : "Convert and edit"}
            </Button>
          )}
        </WizardActions>
      </WizardForm>
    </div>
  );
}
