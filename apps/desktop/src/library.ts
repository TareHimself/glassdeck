import type { LibraryError, Sizing } from "./bindings";

export function describeLibraryError(error: LibraryError): string {
  switch (error.kind) {
    case "invalid_title":
    case "invalid_id":
    case "invalid_size":
    case "io":
      return error.reason;
    case "already_exists":
      return `A package with the id ${error.id} already exists`;
    case "not_found":
      return `No widget ${error.id} in the library`;
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
