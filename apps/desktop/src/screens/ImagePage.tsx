import { noteSaved, useApplyTheme } from "../applying";
import { ApplyToKeyboard } from "../components/ApplyToKeyboard";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { commands, type ImageBackground, type LibraryError } from "../bindings";
import { Button } from "../components/Button";
import { Loading } from "../components/Loading";
import { PageHeader, useBack } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";
import { chooseLabel, usePicker } from "../picking";
import styles from "./VideoPage.module.css";

const LIBRARY = { to: "/backgrounds", label: "Backgrounds" };

/** An image background: the picture as the keyboard shows it, and a way to put another in its place. */
export function ImagePage({ id }: { id: string }) {
  const back = useBack(LIBRARY);
  const [loaded, setLoaded] = useState<{ image: ImageBackground; at: number } | null>(null);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const applying = useApplyTheme();
  const replacing = usePicker(commands.pickImage, (source) => commands.replaceImage(id, source));
  const replaced = replacing.picking.state === "chosen" ? replacing.picking.chosen.value : null;

  useEffect(() => {
    void commands.readImage(id).then((result) => {
      if (result.status === "ok") setLoaded({ image: result.data, at: Date.now() });
      else setProblem(result.error);
    });
  }, [id]);

  useEffect(() => {
    if (!replaced) return;
    setLoaded({ image: replaced, at: Date.now() });
    noteSaved(id);
  }, [replaced, id]);

  if (!loaded) return problem ? <Problem>{describeLibraryError(problem)}</Problem> : <PageHeader back={back} title="" />;
  const { background, record, path, width, height } = loaded.image;

  return (
    <div className={styles.page}>
      <PageHeader
        back={back}
        title={background.title}
        id={`${background.id} ${background.version}, image background`}
        actions={<ApplyToKeyboard id={id} unsaved={false} save={async () => true} apply={applying} />}
      />

      {problem && <Problem>{describeLibraryError(problem)}</Problem>}
      {applying.problem && <Problem>{applying.problem}</Problem>}
      {replacing.problem && <Problem>{describeLibraryError(replacing.problem)}</Problem>}

      <div className={styles.columns}>
        <section aria-label="Preview" className={styles.preview}>
          <div className={styles.stage}>
            <img className={styles.video} alt={background.title} src={`${convertFileSrc(path)}?at=${loaded.at}`} />
            {replacing.picking.state === "loading" && <Loading label="Fitting the new picture to the keyboard" />}
          </div>
        </section>

        <aside className={styles.aside}>
          <section className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Image</h2>
              <Button className={styles.small} disabled={replacing.busy} onClick={() => void replacing.choose()}>
                {chooseLabel(replacing.picking, "Replace file")}
              </Button>
            </div>
            <div className={styles.facts}>
              <span className={styles.muted}>Size</span>
              <span>
                {width} × {height}
              </span>
              <span className={styles.muted}>From</span>
              <span className={styles.from}>{record.from}</span>
            </div>
          </section>
        </aside>
      </div>
    </div>
  );
}
