import { useState, type ReactNode } from "react";
import { useNavigate } from "react-router";
import { commands, GRID, WIDGET_AREA, type Dims, type LibraryError, type Sizing } from "../bindings";
import { Button, ButtonLink } from "../components/Button";
import { Field, Input } from "../components/Input";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { Segments } from "../components/Segments";
import { Stepper } from "../components/Stepper";
import { KeyboardIcon, MonitorIcon, SlidersIcon } from "../icons";
import { describeLibraryError, slug } from "../library";
import styles from "./NewWidget.module.css";

const ID_PREFIX = "local.me";
const STEPS = ["Name", "Parts", "Size"] as const;
type Step = 0 | 1 | 2;

type Mode = Sizing["kind"];
type Bound = "min" | "size" | "max";

type Draft = {
  title: string;
  options: boolean;
  runsOnHost: boolean;
  mode: Mode;
  dims: Record<Bound, Dims>;
};

const START: Draft = {
  title: "",
  options: true,
  runsOnHost: true,
  mode: "range",
  dims: { min: { w: 160, h: 80 }, size: { w: 320, h: 160 }, max: { w: 640, h: 240 } },
};

const MODES: { value: Mode; name: string }[] = [
  { value: "fixed", name: "Fixed" },
  { value: "range", name: "Range" },
  { value: "any", name: "Any size" },
];

function idOf(draft: Draft): string {
  return `${ID_PREFIX}.${slug(draft.title) || "widget"}`;
}

function qualifiedIdOf(draft: Draft): string {
  return `widget.${idOf(draft)}`;
}

function sizingOf(draft: Draft): Sizing {
  const { min, size, max } = draft.dims;
  switch (draft.mode) {
    case "fixed":
      return { kind: "fixed", size };
    case "range":
      return { kind: "range", size, min, max };
    case "any":
      return { kind: "any", size };
  }
}

function stepOf(error: LibraryError): Step {
  return error.kind === "invalid_size" ? 2 : 0;
}

function nudge(dims: Dims, axis: keyof Dims, by: number): Dims {
  const limit = axis === "w" ? WIDGET_AREA.w : WIDGET_AREA.h;
  return { ...dims, [axis]: Math.min(limit, Math.max(GRID, dims[axis] + by)) };
}

function boundsOf(mode: Mode): { bound: Bound; name: string }[] {
  switch (mode) {
    case "fixed":
      return [{ bound: "size", name: "Size" }];
    case "range":
      return [
        { bound: "min", name: "Min" },
        { bound: "size", name: "Default" },
        { bound: "max", name: "Max" },
      ];
    case "any":
      return [{ bound: "size", name: "Default" }];
  }
}

function boxStyle({ w, h }: Dims) {
  return {
    left: `${((WIDGET_AREA.w - w) / 2 / WIDGET_AREA.w) * 100}%`,
    width: `${(w / WIDGET_AREA.w) * 100}%`,
    height: `${(h / WIDGET_AREA.h) * 100}%`,
  };
}

function PartRow(props: { icon: ReactNode; name: string; file: string; checked: boolean; locked?: boolean; onChange?: () => void }) {
  return (
    <label className={styles.part} data-on={(props.checked && !props.locked) || undefined} data-locked={props.locked || undefined}>
      <input type="checkbox" checked={props.checked} disabled={props.locked} onChange={props.onChange} />
      {props.icon}
      <span className={styles.partName}>{props.name}</span>
      <span className={styles.file}>{props.file}</span>
    </label>
  );
}

function BoundEditor(props: { name: string; dims: Dims; main: boolean; onChange: (dims: Dims) => void }) {
  const row = (axis: keyof Dims, less: string, more: string) => (
    <Stepper
      value={`${props.dims[axis]} ${axis}`}
      lessLabel={less}
      moreLabel={more}
      onLess={() => props.onChange(nudge(props.dims, axis, -GRID))}
      onMore={() => props.onChange(nudge(props.dims, axis, GRID))}
    />
  );
  return (
    <fieldset className={styles.bound} data-main={props.main || undefined}>
      <legend>{props.name}</legend>
      {row("w", "Narrower", "Wider")}
      {row("h", "Shorter", "Taller")}
    </fieldset>
  );
}

function AreaPreview({ draft }: { draft: Draft }) {
  const { min, size, max } = draft.dims;
  const outer = draft.mode === "any" ? WIDGET_AREA : draft.mode === "range" ? max : null;
  return (
    <div className={styles.preview}>
      <div className={styles.previewLabel}>
        <span>Widget area</span>
        <span>
          {size.w} × {size.h}
        </span>
      </div>
      <div className={styles.area} style={{ aspectRatio: `${WIDGET_AREA.w} / ${WIDGET_AREA.h}` }}>
        {outer && <div className={styles.box} data-role="max" style={boxStyle(outer)} />}
        {draft.mode === "range" && <div className={styles.box} data-role="min" style={boxStyle(min)} />}
        <div className={styles.box} data-role="size" style={boxStyle(size)} />
      </div>
    </div>
  );
}

export function NewWidget() {
  const navigate = useNavigate();
  const [step, setStep] = useState<Step>(0);
  const [draft, setDraft] = useState<Draft>(START);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);

  const update = (change: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...change }));
    setProblem(null);
  };

  const create = async () => {
    setCreating(true);
    const result = await commands.createWidget({
      title: draft.title,
      id: idOf(draft),
      options: draft.options,
      runs_on_host: draft.runsOnHost,
      sizing: sizingOf(draft),
    });
    setCreating(false);
    if (result.status === "ok") {
      navigate(`/widgets/${result.data.id}`);
    } else {
      setProblem(result.error);
      setStep(stepOf(result.error));
    }
  };

  return (
    <div className={styles.page}>
      <PageHeader back={{ to: "/widgets", label: "Widgets" }} title="New widget" />

      <ol aria-label="Steps" className={styles.steps}>
        {STEPS.map((name, i) => (
          <li key={name}>
            <button type="button" className={styles.step} aria-current={i === step ? "step" : undefined} onClick={() => setStep(i as Step)}>
              <span className={styles.number} data-reached={i <= step || undefined}>
                {i + 1}
              </span>
              {name}
            </button>
          </li>
        ))}
      </ol>

      <section className={styles.form}>
        {step === 0 && (
          <>
            <Field label="Title" htmlFor="ww-title">
              <Input id="ww-title" type="text" value={draft.title} autoFocus onChange={(e) => update({ title: e.target.value })} />
            </Field>
            <Field label="Package id" htmlFor="ww-id">
              <Input id="ww-id" type="text" variant="mono" value={qualifiedIdOf(draft)} readOnly />
            </Field>
          </>
        )}

        {step === 1 && (
          <div className={styles.parts}>
            <PartRow icon={<KeyboardIcon size={22} />} name="Runs on the keyboard" file="keyboard.luau" checked locked />
            <PartRow icon={<SlidersIcon size={22} />} name="Options" file="options.luau" checked={draft.options} onChange={() => update({ options: !draft.options })} />
            <PartRow
              icon={<MonitorIcon size={22} />}
              name="Runs on this device"
              file="host.luau"
              checked={draft.runsOnHost}
              onChange={() => update({ runsOnHost: !draft.runsOnHost })}
            />
          </div>
        )}

        {step === 2 && (
          <>
            <Segments label="Resizing" options={MODES} value={draft.mode} onChange={(mode) => update({ mode })} />
            <div className={styles.bounds}>
              {boundsOf(draft.mode).map(({ bound, name }) => (
                <BoundEditor
                  key={bound}
                  name={name}
                  dims={draft.dims[bound]}
                  main={bound === "size"}
                  onChange={(dims) => update({ dims: { ...draft.dims, [bound]: dims } })}
                />
              ))}
            </div>
            <AreaPreview draft={draft} />
          </>
        )}

        {problem && <Problem>{describeLibraryError(problem)}</Problem>}

        <div className={styles.actions}>
          {step === 0 ? (
            <ButtonLink variant="quiet" to="/widgets">
              Cancel
            </ButtonLink>
          ) : (
            <Button variant="quiet" onClick={() => setStep((step - 1) as Step)}>
              Back
            </Button>
          )}
          {step < 2 ? (
            <Button variant="primary" onClick={() => setStep((step + 1) as Step)}>
              Next
            </Button>
          ) : (
            <Button variant="primary" disabled={creating} onClick={() => void create()}>
              Create and edit
            </Button>
          )}
        </div>
      </section>
    </div>
  );
}
