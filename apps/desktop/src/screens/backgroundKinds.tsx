import type { BackgroundKind } from "../bindings";

export const KIND_NAMES: Record<BackgroundKind, string> = { canvas: "Canvas", video: "Video", image: "Image" };

export function CanvasIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="#c79bff" strokeWidth="1.8">
      <path d="M7 5l-4 5 4 5M13 5l4 5-4 5" />
    </svg>
  );
}

export function VideoIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="#f5b678" strokeWidth="1.8">
      <rect x="2.5" y="4" width="15" height="12" rx="2" />
      <path d="M8 7.5v5l4.5-2.5z" fill="#f5b678" />
    </svg>
  );
}

export function ImageIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 20 20" aria-hidden="true" fill="none" stroke="#7fd4a8" strokeWidth="1.8">
      <rect x="2.5" y="4" width="15" height="12" rx="2" />
      <circle cx="7.5" cy="8.5" r="1.5" fill="#7fd4a8" stroke="none" />
      <path d="M3.5 15l4.5-4.5 3 3 2.5-2.5 3.5 3.5" />
    </svg>
  );
}
