import { useEffect, useState } from "react";
import { Link } from "react-router";
import { commands, type OptionsPanel, type ThemeWidget, type Value, type WidgetSummary } from "../bindings";
import { describeLibraryError } from "../library";
import { OptionControls } from "./OptionControls";
import styles from "./SelectedWidget.module.css";

const OPTIONS_SCRIPT = "options.luau";

/** The selected widget's `options.luau`, read once per widget. */
function useOptionsSource(id: string): { state: "loading" } | { state: "ready"; source: string } | { state: "failed"; reason: string } {
  const [source, setSource] = useState<ReturnType<typeof useOptionsSource>>({ state: "loading" });
  useEffect(() => {
    let current = true;
    setSource({ state: "loading" });
    void commands.readWidget(id).then((result) => {
      if (!current) return;
      if (result.status === "error") return setSource({ state: "failed", reason: describeLibraryError(result.error) });
      setSource({ state: "ready", source: result.data.files.find((f) => f.name === OPTIONS_SCRIPT)?.text ?? "" });
    });
    return () => {
      current = false;
    };
  }, [id]);
  return source;
}

function Options({ id, chosen, onChange }: { id: string; chosen: Record<string, Value>; onChange: (chosen: Record<string, Value>) => void }) {
  const source = useOptionsSource(id);
  const [panel, setPanel] = useState<OptionsPanel | null>(null);
  const chosenKey = JSON.stringify(chosen);
  useEffect(() => {
    if (source.state !== "ready") return;
    let current = true;
    void commands.widgetOptions(source.source, chosen).then((p) => current && setPanel(p));
    return () => {
      current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [source, chosenKey]);

  if (source.state === "failed") return <div className={styles.failed}>{source.reason}</div>;
  if (!panel) return null;
  if (panel.state === "failed") return <div className={styles.failed}>{panel.reason}</div>;
  if (panel.items.length === 0) return <div className={styles.none}>This widget has no options.</div>;
  return <OptionControls items={panel.items} chosen={chosen} onChange={(path, value) => onChange({ ...chosen, [path]: value })} />;
}

export function SelectedWidget({
  widget,
  summary,
  onOptions,
}: {
  widget: ThemeWidget;
  summary: WidgetSummary | undefined;
  onOptions: (chosen: Record<string, Value>) => void;
}) {
  return (
    <section className={styles.card}>
      <div className={styles.head}>
        <div className={styles.names}>
          <h2 className={styles.title}>{summary?.title ?? widget.package}</h2>
          <div className={styles.id}>
            {widget.package}
            {summary && ` ${summary.version}`}
          </div>
        </div>
        {summary && (
          <Link className={styles.edit} to={`/widgets/${widget.package}`}>
            Edit
          </Link>
        )}
      </div>
      {!summary ? (
        <div className={styles.failed}>{widget.package} isn't in the library</div>
      ) : summary.has_options ? (
        <Options id={widget.package} chosen={widget.options ?? {}} onChange={onOptions} />
      ) : (
        <div className={styles.none}>This widget has no options.</div>
      )}
    </section>
  );
}
