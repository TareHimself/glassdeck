import { useEffect, useState } from "react";
import { BUILT_IN_BACKGROUND, commands, type BackgroundSummary, type LibraryError, type Sizing } from "./bindings";

/** The built-in gradient, offered beside the library's backgrounds; it has no folder, so no summary of its own. */
export const BUILT_IN: BackgroundSummary = { id: BUILT_IN_BACKGROUND, title: "Gradient", version: "", kind: "canvas", preview: null };

/** The built-in gradient, with its still once it's drawn. */
export function useBuiltInBackground(): BackgroundSummary {
  const [builtIn, setBuiltIn] = useState<BackgroundSummary>(BUILT_IN);
  useEffect(() => {
    void commands.builtInStill().then((r) => r.status === "ok" && setBuiltIn({ ...BUILT_IN, preview: r.data }));
  }, []);
  return builtIn;
}

export function describeLibraryError(error: LibraryError): string {
  switch (error.kind) {
    case "invalid_title":
    case "invalid_id":
    case "invalid_size":
    case "invalid_version":
    case "invalid_time":
    case "checker_unavailable":
    case "io":
      return error.reason;
    case "already_exists":
      return `A package with the id ${error.id} already exists`;
    case "not_found":
      return `Nothing with the id ${error.id} in the library`;
    case "bad_video":
      return error.reason;
    case "not_a_widget":
      return `That .deck can't be imported: ${error.reason}`;
    case "unreadable":
      return `${error.folder} can't be read: ${error.reason}`;
  }
}

export function sizeLabel(sizing: Sizing): string {
  const { w, h } = sizing.size;
  return `${w}×${h}`;
}

/** Lowercase, with runs of anything else as one `-`, as PackageId::from_name makes it. */
export function slug(title: string): string {
  return title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
