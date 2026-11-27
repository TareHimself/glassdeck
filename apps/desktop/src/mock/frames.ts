/** Stand-ins for what the native previews draw, as base64 images like theirs. */

function base64Of(canvas: HTMLCanvasElement, type: "image/png" | "image/jpeg"): string {
  return canvas.toDataURL(type, 0.8).split(",")[1] ?? "";
}

function canvasOf(w: number, h: number): [HTMLCanvasElement, CanvasRenderingContext2D] {
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("mock frames: no 2D canvas context");
  return [canvas, context];
}

/** A widget's tile: its title on a translucent card, as base64 PNG. */
export function widgetTile(title: string, w: number, h: number, seconds: number): string {
  const [canvas, g] = canvasOf(w, h);
  g.fillStyle = "rgba(8, 6, 24, 0.55)";
  g.beginPath();
  g.roundRect(0, 0, w, h, Math.min(w, h) * 0.12);
  g.fill();
  g.fillStyle = "#ffffff";
  g.font = `600 ${Math.round(h * 0.18)}px Inter, sans-serif`;
  g.textAlign = "center";
  g.textBaseline = "middle";
  g.fillText(title, w / 2, h * 0.42);
  g.globalAlpha = 0.7;
  g.font = `${Math.round(h * 0.12)}px Inter, sans-serif`;
  g.fillText(`${seconds}s`, w / 2, h * 0.68);
  return base64Of(canvas, "image/png");
}

/** A canvas background's frame at `time` seconds, as base64 JPEG. */
export function canvasFrame(seed: string, time: number): string {
  const [canvas, g] = canvasOf(480, 270);
  const hue = [...seed].reduce((sum, c) => sum + c.charCodeAt(0), 0) % 360;
  const x = 240 + 160 * Math.sin(time * 0.6);
  const glow = g.createRadialGradient(x, 120, 10, x, 120, 260);
  glow.addColorStop(0, `hsl(${hue} 70% 60%)`);
  glow.addColorStop(1, `hsl(${(hue + 60) % 360} 60% 12%)`);
  g.fillStyle = glow;
  g.fillRect(0, 0, 480, 270);
  return base64Of(canvas, "image/jpeg");
}
