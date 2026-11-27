import type { OptionView, PanelItem, Value } from "../bindings";
import { Select } from "../components/Select";
import styles from "./OptionControls.module.css";

export type Chosen = { [path: string]: Value };

function Control({ option, onChange }: { option: OptionView; onChange: (value: Value) => void }) {
  const id = `option-${option.path}`;
  const kind = option.kind;
  switch (kind.type) {
    case "toggle":
      return (
        <label className={styles.row}>
          <span>{option.label}</span>
          <input id={id} type="checkbox" checked={option.value === true} onChange={(e) => onChange(e.target.checked)} />
        </label>
      );
    case "range": {
      const value = typeof option.value === "number" ? option.value : (kind.default ?? 0);
      return (
        <div className={styles.range}>
          <div className={styles.rangeHead}>
            <label htmlFor={id}>{option.label}</label>
            <span className={styles.rangeValue}>
              {value}
              {kind.unit ? ` ${kind.unit}` : ""}
            </span>
          </div>
          <input
            id={id}
            type="range"
            min={kind.min ?? 0}
            max={kind.max ?? 1}
            step={kind.step ?? "any"}
            value={value}
            onChange={(e) => onChange(Number(e.target.value))}
          />
        </div>
      );
    }
    case "number":
      return (
        <label className={styles.row}>
          <span>
            {option.label}
            {kind.unit ? ` (${kind.unit})` : ""}
          </span>
          <input
            id={id}
            type="number"
            min={kind.min ?? undefined}
            max={kind.max ?? undefined}
            step={kind.step ?? "any"}
            value={typeof option.value === "number" ? option.value : ""}
            onChange={(e) => onChange(Number(e.target.value))}
          />
        </label>
      );
    case "text":
      return (
        <label className={styles.row}>
          <span>{option.label}</span>
          <input
            id={id}
            type="text"
            placeholder={kind.placeholder ?? undefined}
            maxLength={kind.max_length ?? undefined}
            value={typeof option.value === "string" ? option.value : ""}
            onChange={(e) => onChange(e.target.value)}
          />
        </label>
      );
    case "choice":
      return (
        <div className={styles.row}>
          <label htmlFor={id}>{option.label}</label>
          <Select
            id={id}
            className={styles.choice}
            value={typeof option.value === "string" ? option.value : kind.default}
            onChange={onChange}
            options={kind.values.map((v) => ({ value: v, label: v }))}
          />
        </div>
      );
    case "color": {
      const rgba = typeof option.value === "string" ? option.value : kind.default;
      return (
        <label className={styles.row}>
          <span>{option.label}</span>
          <input id={id} type="color" value={rgba.slice(0, 7)} onChange={(e) => onChange(`${e.target.value}${rgba.slice(7, 9) || "ff"}`)} />
        </label>
      );
    }
  }
}

/** Chosen values show at once; the panel from `options.luau` catches up after its next run. */
export function OptionControls({ items, chosen, onChange }: { items: PanelItem[]; chosen: Chosen; onChange: (path: string, value: Value) => void }) {
  return (
    <div className={styles.list}>
      {items.map((item) => {
        if (item.item === "heading") {
          return (
            <div key={`heading-${item.text}`} className={styles.heading}>
              {item.text}
            </div>
          );
        }
        if (item.item === "section") {
          return (
            <div key={item.path} className={styles.section}>
              <div className={styles.heading}>{item.label}</div>
              <OptionControls items={item.items} chosen={chosen} onChange={onChange} />
            </div>
          );
        }
        if (!item.visible) return null;
        return (
          <div key={item.path}>
            <Control option={item.path in chosen ? { ...item, value: chosen[item.path] ?? item.value } : item} onChange={(value) => onChange(item.path, value)} />
            {item.hint && <div className={styles.hint}>{item.hint}</div>}
          </div>
        );
      })}
    </div>
  );
}
