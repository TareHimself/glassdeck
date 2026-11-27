import { GRID, WIDGET_AREA } from "./bindings";

/** A rectangle in the widget area, in panel pixels. */
export type Box = { x: number; y: number; w: number; h: number };

export function overlaps(a: Box, b: Box): boolean {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

/** The first grid spot, top row first, where a widget of `size` fits without overlapping `taken`. */
export function freeSpot(size: { w: number; h: number }, taken: Box[]): Box | null {
  for (let y = 0; y + size.h <= WIDGET_AREA.h; y += GRID) {
    for (let x = 0; x + size.w <= WIDGET_AREA.w; x += GRID) {
      const box = { x, y, ...size };
      if (!taken.some((t) => overlaps(box, t))) return box;
    }
  }
  return null;
}
