import { useEffect, useState } from "react";
import { useParams } from "react-router";
import { commands, type LibraryError, type WidgetSummary } from "../bindings";
import { PageHeader } from "../components/PageHeader";
import { Problem } from "../components/Problem";
import { describeLibraryError } from "../library";

type Found = { state: "loading" } | { state: "found"; widget: WidgetSummary } | { state: "missing" } | { state: "failed"; error: LibraryError };

const BACK = { to: "/widgets", label: "Widgets" };

export function WidgetEditor() {
  const { id = "" } = useParams();
  const [found, setFound] = useState<Found>({ state: "loading" });

  useEffect(() => {
    void commands.listWidgets().then((result) => {
      if (result.status === "error") return setFound({ state: "failed", error: result.error });
      const widget = result.data.find((w) => w.id === id);
      setFound(widget ? { state: "found", widget } : { state: "missing" });
    });
  }, [id]);

  switch (found.state) {
    case "loading":
      return <PageHeader back={BACK} title="" />;
    case "found":
      return <PageHeader back={BACK} title={found.widget.title} id={`${found.widget.id} ${found.widget.version}`} />;
    case "missing":
      return (
        <>
          <PageHeader back={BACK} title="" />
          <Problem>No widget {id} in the library</Problem>
        </>
      );
    case "failed":
      return (
        <>
          <PageHeader back={BACK} title="" />
          <Problem>{describeLibraryError(found.error)}</Problem>
        </>
      );
  }
}
