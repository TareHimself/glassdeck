import { useEffect, useState } from "react";
import { useParams } from "react-router";
import { commands, type BackgroundSummary, type LibraryError } from "../bindings";
import { PageHeader, useBack } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";
import { CanvasEditor } from "./CanvasEditor";
import { VideoPage } from "./VideoPage";

type Loaded = { state: "loading" } | { state: "ready"; background: BackgroundSummary } | { state: "failed"; error: LibraryError };

const LIBRARY = { to: "/backgrounds", label: "Backgrounds" };

/** Opens a background in the editor for its kind. */
export function BackgroundEditor() {
  const back = useBack(LIBRARY);
  const id = useParams().id ?? "";
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });

  useEffect(() => {
    void commands.listBackgrounds().then((result) => {
      if (result.status === "error") return setLoaded({ state: "failed", error: result.error });
      const background = result.data.find((b) => b.id === id);
      setLoaded(background ? { state: "ready", background } : { state: "failed", error: { kind: "not_found", id } });
    });
  }, [id]);

  switch (loaded.state) {
    case "loading":
      return <PageHeader back={back} title="" />;
    case "failed":
      return (
        <>
          <PageHeader back={back} title="" />
          <Problem>{describeLibraryError(loaded.error)}</Problem>
        </>
      );
    case "ready": {
      const { background } = loaded;
      if (background.kind === "canvas") return <CanvasEditor id={id} />;
      return <VideoPage id={id} />;
    }
  }
}
