import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { commands, type ImageBackground, type LibraryError } from "../bindings";
import { Button } from "../components/Button";
import { PageHeader, useBack } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";
import styles from "./VideoPage.module.css";

const LIBRARY = { to: "/backgrounds", label: "Backgrounds" };

/** An image background: the picture as the keyboard shows it, and a way to put another in its place. */
export function ImagePage({ id }: { id: string }) {
  const back = useBack(LIBRARY);
  const [loaded, setLoaded] = useState<{ image: ImageBackground; at: number } | null>(null);
  const [problem, setProblem] = useState<LibraryError | null>(null);
  const [replacing, setReplacing] = useState(false);

  useEffect(() => {
    void commands.readImage(id).then((result) => {
      if (result.status === "ok") setLoaded({ image: result.data, at: Date.now() });
      else setProblem(result.error);
    });
  }, [id]);

  if (!loaded) return problem ? <Problem>{describeLibraryError(problem)}</Problem> : <PageHeader back={back} title="" />;
  const { background, record, path, width, height } = loaded.image;

  const replace = async () => {
    const source = await commands.pickImage();
    if (!source) return;
    setReplacing(true);
    const result = await commands.replaceImage(id, source);
    setReplacing(false);
    if (result.status === "error") return setProblem(result.error);
    setProblem(null);
    setLoaded({ image: result.data, at: Date.now() });
  };

  return (
    <div className={styles.page}>
      <PageHeader back={back} title={background.title} id={`${background.id} ${background.version}, image background`} />

      {problem && <Problem>{describeLibraryError(problem)}</Problem>}

      <div className={styles.columns}>
        <section aria-label="Preview" className={styles.preview}>
          <div className={styles.stage}>
            <img className={styles.video} alt={background.title} src={`${convertFileSrc(path)}?at=${loaded.at}`} />
          </div>
        </section>

        <aside className={styles.aside}>
          <section className={styles.card}>
            <div className={styles.cardHead}>
              <h2 className={styles.cardTitle}>Image</h2>
              <Button className={styles.small} disabled={replacing} onClick={() => void replace()}>
                Replace file
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
