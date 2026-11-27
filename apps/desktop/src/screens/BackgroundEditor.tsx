import { useEffect, useState } from "react";
import { useParams } from "react-router";
import { commands, type BackgroundSummary, type LibraryError } from "../bindings";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";

type Loaded = { state: "loading" } | { state: "ready"; background: BackgroundSummary } | { state: "failed"; error: LibraryError };

export function BackgroundEditor() {
  const id = useParams().id ?? "";
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });

  useEffect(() => {
    void commands.listBackgrounds().then((result) => {
      if (result.status === "error") return setLoaded({ state: "failed", error: result.error });
      const background = result.data.find((b) => b.id === id);
      setLoaded(background ? { state: "ready", background } : { state: "failed", error: { kind: "not_found", id } });
    });
  }, [id]);

  if (loaded.state === "loading") return null;
  if (loaded.state === "failed") {
    return (
      <>
        <PageHeader back={{ to: "/backgrounds", label: "Backgrounds" }} title="Background" />
        <Problem>{describeLibraryError(loaded.error)}</Problem>
      </>
    );
  }
  const { background } = loaded;
  return (
    <PageHeader
      back={{ to: "/backgrounds", label: "Backgrounds" }}
      title={background.title}
      id={`${background.id} ${background.version}, ${background.kind} background`}
    />
  );
}
