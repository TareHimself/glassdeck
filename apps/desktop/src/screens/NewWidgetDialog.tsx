import { useState } from "react";
import { useNavigate } from "react-router";
import { commands, GRID, WIDGET_AREA, type Dims, type LibraryError, type Sizing } from "../bindings";
import { Button } from "../components/Button";
import { Dialog, DialogActions } from "../components/Dialog";
import { Field, Input } from "../components/Input";
import { Problem } from "../components/Problem";
import { Segments } from "../components/Segments";
import { describeLibraryError, slug } from "../library";
import styles from "./NewWidgetDialog.module.css";

const ID_PREFIX = "local.me";

type Mode = Sizing["kind"];
type Bound = "min" | "size" | "max";

const MODES: { value: Mode; name: string }[] = [
  { value: "fixed", name: "Fixed" },
  { value: "range", name: "Range" },
  { value: "any", name: "Any size" },
];

const START: Record<Bound, Dims> = { min: { w: 160, h: 80 }, size: { w: 320, h: 160 }, max: { w: 640, h: 240 } };

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

function sizingOf(mode: Mode, dims: Record<Bound, Dims>): Sizing {
  switch (mode) {
    case "fixed":
      return { kind: "fixed", size: dims.size };
    case "range":
      return { kind: "range", size: dims.size, min: dims.min, max: dims.max };
    case "any":
      return { kind: "any", size: dims.size };
  }
}

/** A box of `dims`, centred in the widget area, as percentages of it. */
function centred({ w, h }: Dims) {
  return {
    left: `${((WIDGET_AREA.w - w) / 2 / WIDGET_AREA.w) * 100}%`,
    top: `${((WIDGET_AREA.h - h) / 2 / WIDGET_AREA.h) * 100}%`,
    width: `${(w / WIDGET_AREA.w) * 100}%`,
    height: `${(h / WIDGET_AREA.h) * 100}%`,
  };
}

/** Whole grid steps, inside the widget area. */
function snapped(value: number, limit: number): number {
  return Math.min(limit, Math.max(GRID, Math.round(value / GRID) * GRID));
}

function DimsInput({ name, dims, onChange }: { name: string; dims: Dims; onChange: (dims: Dims) => void }) {
  const [draft, setDraft] = useState<Dims>(dims);
  const commit = (axis: keyof Dims, limit: number) => {
    const next = { ...dims, [axis]: snapped(draft[axis], limit) };
    setDraft(next);
    onChange(next);
  };
  const field = (axis: keyof Dims, label: string, limit: number) => (
    <input
      type="number"
      className={styles.number}
      aria-label={`${name} ${label}`}
      step={GRID}
      value={draft[axis]}
      onChange={(e) => setDraft({ ...draft, [axis]: Number(e.target.value) })}
      onBlur={() => commit(axis, limit)}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit(axis, limit);
      }}
    />
  );
  return (
    <div className={styles.row}>
      <span className={styles.rowName}>{name}</span>
      {field("w", "width", WIDGET_AREA.w)}
      <span className={styles.times}>×</span>
      {field("h", "height", WIDGET_AREA.h)}
    </div>
  );
}

export function NewWidgetDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate();
  const [title, setTitle] = useState("");
  const [mode, setMode] = useState<Mode>("range");
  const [dims, setDims] = useState(START);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [creating, setCreating] = useState(false);
  const id = `${ID_PREFIX}.${slug(title) || "widget"}`;

  const create = async () => {
    setCreating(true);
    const result = await commands.createWidget({ title, id, sizing: sizingOf(mode, dims) });
    setCreating(false);
    if (result.status === "ok") navigate(`/widgets/${result.data.id}`);
    else setProblem(result.error);
  };

  return (
    <Dialog title="New widget" width={600} onClose={onClose}>
      <div className={styles.area} style={{ aspectRatio: `${WIDGET_AREA.w} / ${WIDGET_AREA.h}` }}>
        {mode === "range" && <div className={styles.bound} style={centred(dims.max)} />}
        {mode === "range" && <div className={styles.bound} style={centred(dims.min)} />}
        <div className={styles.box} style={centred(dims.size)}>
          {title}
        </div>
      </div>

      <Field label="Title" htmlFor="nw-title">
        <Input
          id="nw-title"
          type="text"
          value={title}
          autoFocus
          onChange={(e) => {
            setTitle(e.target.value);
            setProblem(null);
          }}
        />
        <span className={styles.id}>widget.{id}</span>
      </Field>

      <div className={styles.size}>
        <div className={styles.sizeHead}>
          <span className={styles.label}>Size</span>
          <Segments compact label="Resizing" options={MODES} value={mode} onChange={setMode} />
        </div>
        {boundsOf(mode).map(({ bound, name }) => (
          <DimsInput key={bound} name={name} dims={dims[bound]} onChange={(next) => setDims({ ...dims, [bound]: next })} />
        ))}
      </div>

      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <DialogActions>
        <Button variant="quiet" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" disabled={creating} onClick={() => void create()}>
          Create
        </Button>
      </DialogActions>
    </Dialog>
  );
}
