import type { BackgroundKind } from "../bindings";

export const KIND_NAMES: Record<BackgroundKind, string> = { canvas: "Canvas", video: "Video" };

export function CanvasIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="#c79bff" strokeWidth="1.8">
      <path d="M7 5l-4 5 4 5M13 5l4 5-4 5" />
    </svg>
  );
}

export function KindIcon({ kind }: { kind: BackgroundKind }) {
  return kind === "canvas" ? <CanvasIcon /> : <VideoIcon />;
}

export function VideoIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="#f5b678" strokeWidth="1.8">
      <rect x="2.5" y="4" width="15" height="12" rx="2" />
      <path d="M8 7.5v5l4.5-2.5z" fill="#f5b678" />
    </svg>
  );
}
