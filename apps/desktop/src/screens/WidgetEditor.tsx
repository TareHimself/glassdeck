import { css } from "@linaria/core";
import { styled } from "@linaria/react";
import { useEffect, useState } from "react";
import { Link, useParams } from "react-router";
import { commands, type LibraryError, type WidgetSummary } from "../bindings";
import { describeLibraryError } from "../library";
import { Problem, Title } from "../ui";

const back = css`
  font-size: 13px;
  text-decoration: none;
`;

const Id = styled.div`
  margin-top: 2px;
  font-family: var(--mono);
  font-size: 12px;
  color: var(--muted);
`;

type Found = { state: "loading" } | { state: "found"; widget: WidgetSummary } | { state: "missing" } | { state: "failed"; error: LibraryError };

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

  return (
    <header>
      <Link className={back} to="/widgets">
        ← Widgets
      </Link>
      {found.state === "found" && (
        <>
          <Title>{found.widget.title}</Title>
          <Id>
            {found.widget.id} {found.widget.version}
          </Id>
        </>
      )}
      {found.state === "missing" && <Problem role="alert">No widget {id} in the library</Problem>}
      {found.state === "failed" && <Problem role="alert">{describeLibraryError(found.error)}</Problem>}
    </header>
  );
}
