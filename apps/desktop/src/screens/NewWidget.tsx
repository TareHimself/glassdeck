import { css } from "@linaria/core";
import { styled } from "@linaria/react";
import { useState, type ReactNode } from "react";
import { Link, useNavigate } from "react-router";
import { commands, GRID, WIDGET_AREA, type Dims, type LibraryError, type Sizing } from "../bindings";
import { KeyboardIcon, MonitorIcon, SlidersIcon } from "../icons";
import { describeLibraryError, slug } from "../library";
import { Input, Label, Problem, Title, primary, quiet } from "../ui";

const ID_PREFIX = "local";
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

const MODES: { mode: Mode; name: string }[] = [
  { mode: "fixed", name: "Fixed" },
  { mode: "range", name: "Range" },
  { mode: "any", name: "Any size" },
];

function idOf(draft: Draft): string {
  return `${ID_PREFIX}.${slug(draft.title) || "widget"}`;
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

const back = css`
  font-size: 13px;
  text-decoration: none;
`;

const Page = styled.div`
  display: flex;
  flex-direction: column;
  gap: 20px;
`;

const Steps = styled.ol`
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
`;

const StepButton = styled.button`
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 44px;
  padding: 0 16px 0 8px;
  border-radius: 999px;
  border: 1px solid var(--line-strong);
  background: var(--surface);
  color: var(--ink);
  font-weight: 500;
  cursor: pointer;

  &[aria-current="step"] {
    border-color: var(--accent);
    background: var(--raised);
  }
`;

const StepNumber = styled.span`
  width: 28px;
  height: 28px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--line);
  color: var(--muted);
  font-size: 13px;
  font-weight: 700;

  &[data-reached] {
    background: var(--accent);
    color: var(--accent-ink);
  }
`;

const Form = styled.section`
  min-width: 0;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 22px;
  display: flex;
  flex-direction: column;
  gap: 18px;
`;

const Field = styled.div`
  display: flex;
  flex-direction: column;
  gap: 6px;
  max-width: 440px;
`;

const Parts = styled.div`
  display: flex;
  flex-direction: column;
  gap: 10px;
  max-width: 520px;
`;

const Part = styled.label`
  display: flex;
  align-items: center;
  gap: 14px;
  min-height: 56px;
  padding: 10px 14px;
  border-radius: 12px;
  border: 1px solid var(--line-strong);
  background: var(--sunken);
  cursor: pointer;

  &[data-on] {
    border-color: var(--accent);
  }

  &[data-locked] {
    cursor: default;
    opacity: 0.7;
  }

  & > input {
    width: 20px;
    height: 20px;
    margin: 0;
    accent-color: var(--accent);
  }

  & > svg {
    flex: none;
    color: var(--accent-soft);
  }
`;

const PartName = styled.span`
  flex: 1;
  font-weight: 600;
`;

const File = styled.span`
  font-family: var(--mono);
  font-size: 13px;
  color: var(--muted);
`;

const Segments = styled.div`
  display: flex;
  align-self: flex-start;
  gap: 4px;
  padding: 4px;
  border-radius: 10px;
  background: var(--sunken);
  border: 1px solid var(--line);
`;

const Segment = styled.button`
  min-height: 40px;
  padding: 0 16px;
  border-radius: 7px;
  border: 0;
  background: transparent;
  color: var(--muted);
  font-weight: 500;
  cursor: pointer;

  &[aria-pressed="true"] {
    background: var(--selected);
    color: var(--ink);
  }
`;

const Bounds = styled.div`
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 14px;
  max-width: 620px;
`;

const BoundBox = styled.fieldset`
  margin: 0;
  padding: 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  border-radius: 12px;
  border: 1px solid var(--line-strong);
  background: var(--sunken);

  &[data-main] {
    border-color: var(--accent);
  }

  & > legend {
    padding: 0 4px;
    font-weight: 600;
  }
`;

const Stepper = styled.div`
  display: flex;
  align-items: center;
  gap: 6px;

  & > span {
    flex: 1;
    text-align: center;
    font-family: var(--mono);
    font-size: 14px;
  }

  & > button {
    width: 36px;
    height: 36px;
    border-radius: var(--radius-small);
    border: 1px solid var(--line-strong);
    background: var(--surface);
    color: var(--ink);
    cursor: pointer;
  }
`;

const Preview = styled.div`
  display: flex;
  flex-direction: column;
  gap: 8px;
`;

const PreviewLabel = styled.div`
  display: flex;
  justify-content: space-between;
  font-size: 13px;
  color: var(--muted);

  & > :last-child {
    font-family: var(--mono);
  }
`;

const Area = styled.div`
  position: relative;
  border-radius: 8px;
  overflow: hidden;
  background:
    radial-gradient(circle at 25% 0%, rgba(214, 107, 120, 0.5), transparent 60%),
    linear-gradient(180deg, #0f0a29, #3d1c73);
`;

const Box = styled.div`
  position: absolute;
  top: 0;
  box-sizing: border-box;
  border-radius: 4px;

  &[data-role="size"] {
    background: rgba(141, 188, 255, 0.28);
    border: 1.5px solid var(--accent-soft);
  }

  &[data-role="min"] {
    border: 1px dashed rgba(141, 188, 255, 0.6);
  }

  &[data-role="max"] {
    border: 1px dashed rgba(255, 255, 255, 0.35);
  }
`;

const Actions = styled.div`
  display: flex;
  justify-content: space-between;
  gap: 8px;
  padding-top: 16px;
  border-top: 1px solid var(--line);
`;

function boxStyle({ w, h }: Dims) {
  return {
    left: `${((WIDGET_AREA.w - w) / 2 / WIDGET_AREA.w) * 100}%`,
    width: `${(w / WIDGET_AREA.w) * 100}%`,
    height: `${(h / WIDGET_AREA.h) * 100}%`,
  };
}

function PartRow(props: { icon: ReactNode; name: string; file: string; checked: boolean; locked?: boolean; onChange?: () => void }) {
  return (
    <Part data-on={(props.checked && !props.locked) || undefined} data-locked={props.locked || undefined}>
      <input type="checkbox" checked={props.checked} disabled={props.locked} onChange={props.onChange} />
      {props.icon}
      <PartName>{props.name}</PartName>
      <File>{props.file}</File>
    </Part>
  );
}

function BoundEditor(props: { name: string; dims: Dims; main: boolean; onChange: (dims: Dims) => void }) {
  const row = (axis: keyof Dims, less: string, more: string) => (
    <Stepper>
      <button type="button" aria-label={less} onClick={() => props.onChange(nudge(props.dims, axis, -GRID))}>
        −
      </button>
      <span>
        {props.dims[axis]} {axis}
      </span>
      <button type="button" aria-label={more} onClick={() => props.onChange(nudge(props.dims, axis, GRID))}>
        +
      </button>
    </Stepper>
  );
  return (
    <BoundBox data-main={props.main || undefined}>
      <legend>{props.name}</legend>
      {row("w", "Narrower", "Wider")}
      {row("h", "Shorter", "Taller")}
    </BoundBox>
  );
}

function AreaPreview({ draft }: { draft: Draft }) {
  const { min, size, max } = draft.dims;
  const outer = draft.mode === "any" ? WIDGET_AREA : draft.mode === "range" ? max : null;
  return (
    <Preview>
      <PreviewLabel>
        <span>Widget area</span>
        <span>
          {size.w} × {size.h}
        </span>
      </PreviewLabel>
      <Area style={{ aspectRatio: `${WIDGET_AREA.w} / ${WIDGET_AREA.h}` }}>
        {outer && <Box data-role="max" style={boxStyle(outer)} />}
        {draft.mode === "range" && <Box data-role="min" style={boxStyle(min)} />}
        <Box data-role="size" style={boxStyle(size)} />
      </Area>
    </Preview>
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
    <Page>
      <header>
        <Link className={back} to="/widgets">
          ← Widgets
        </Link>
        <Title>New widget</Title>
      </header>

      <Steps aria-label="Steps">
        {STEPS.map((name, i) => (
          <li key={name}>
            <StepButton type="button" aria-current={i === step ? "step" : undefined} onClick={() => setStep(i as Step)}>
              <StepNumber data-reached={i <= step || undefined}>{i + 1}</StepNumber>
              {name}
            </StepButton>
          </li>
        ))}
      </Steps>

      <Form>
        {step === 0 && (
          <>
            <Field>
              <Label htmlFor="ww-title">Title</Label>
              <Input id="ww-title" type="text" value={draft.title} autoFocus onChange={(e) => update({ title: e.target.value })} />
            </Field>
            <Field>
              <Label htmlFor="ww-id">Package id</Label>
              <Input id="ww-id" type="text" data-variant="mono" value={idOf(draft)} readOnly />
            </Field>
          </>
        )}

        {step === 1 && (
          <Parts>
            <PartRow icon={<KeyboardIcon size={22} />} name="Runs on the keyboard" file="keyboard.luau" checked locked />
            <PartRow icon={<SlidersIcon size={22} />} name="Options" file="options.luau" checked={draft.options} onChange={() => update({ options: !draft.options })} />
            <PartRow
              icon={<MonitorIcon size={22} />}
              name="Runs on this device"
              file="host.luau"
              checked={draft.runsOnHost}
              onChange={() => update({ runsOnHost: !draft.runsOnHost })}
            />
          </Parts>
        )}

        {step === 2 && (
          <>
            <Segments role="group" aria-label="Resizing">
              {MODES.map(({ mode, name }) => (
                <Segment key={mode} type="button" aria-pressed={draft.mode === mode} onClick={() => update({ mode })}>
                  {name}
                </Segment>
              ))}
            </Segments>
            <Bounds>
              {boundsOf(draft.mode).map(({ bound, name }) => (
                <BoundEditor
                  key={bound}
                  name={name}
                  dims={draft.dims[bound]}
                  main={bound === "size"}
                  onChange={(dims) => update({ dims: { ...draft.dims, [bound]: dims } })}
                />
              ))}
            </Bounds>
            <AreaPreview draft={draft} />
          </>
        )}

        {problem && <Problem role="alert">{describeLibraryError(problem)}</Problem>}

        <Actions>
          {step === 0 ? (
            <Link className={quiet} to="/widgets">
              Cancel
            </Link>
          ) : (
            <button type="button" className={quiet} onClick={() => setStep((step - 1) as Step)}>
              Back
            </button>
          )}
          {step < 2 ? (
            <button type="button" className={primary} onClick={() => setStep((step + 1) as Step)}>
              Next
            </button>
          ) : (
            <button type="button" className={primary} disabled={creating} onClick={() => void create()}>
              Create and edit
            </button>
          )}
        </Actions>
      </Form>
    </Page>
  );
}
