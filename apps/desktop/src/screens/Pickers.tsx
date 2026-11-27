import type { BackgroundSummary, WidgetSummary } from "../bindings";
import { LibraryPicker } from "../components/LibraryPicker";
import { BackgroundThumb, WidgetThumb } from "../components/Thumbs";
import { sizeLabel } from "../library";
import { KindIcon } from "./backgroundKinds";
import styles from "./Pickers.module.css";

type Common<T> = { items: T[]; onPick: (item: T) => void; onNew: () => void; onClose: () => void };

export function BackgroundPicker({ current, ...rest }: Common<BackgroundSummary> & { current: string }) {
  return (
    <LibraryPicker
      {...rest}
      title="Change background"
      searchLabel="Search backgrounds"
      newLabel="New background…"
      keyOf={(b) => b.id}
      searchText={(b) => [b.title, b.id]}
      card={(b) => ({ title: b.title, id: b.id, detail: <KindIcon kind={b.kind} />, current: b.id === current })}
      preview={(b, live) => <BackgroundThumb background={b} live={live} />}
    />
  );
}

export function WidgetPicker(props: Common<WidgetSummary>) {
  return (
    <LibraryPicker
      {...props}
      title="Add widget"
      searchLabel="Search widgets"
      newLabel="New widget…"
      keyOf={(w) => w.id}
      searchText={(w) => [w.title, w.id]}
      card={(w) => ({ title: w.title, id: w.id, detail: <span className={styles.size}>{sizeLabel(w.sizing)}</span> })}
      preview={(w, live) => <WidgetThumb widget={w} live={live} />}
    />
  );
}
