import { expect, test } from "@playwright/test";

/**
 * The built-in gradient's real shader (e2e/fixtures/gradient.essl, Slang's output as packed in
 * builtin/gradient.deck) drawn by the app's WebGL renderer in Chromium, the engine WebView2 runs.
 */
test("the gradient's real Slang shader draws in WebGL2 with its inputs", async ({ page }) => {
  await page.goto("/");
  const center = await page.evaluate(async () => {
    const { CanvasRenderer } = await import("/src/canvasRenderer.ts");
    const fragment: string = (await import("/e2e/fixtures/gradient.essl?raw")).default;
    const canvas = document.createElement("canvas");
    document.body.append(canvas);
    const renderer = new CanvasRenderer(canvas);
    renderer.useProgram(fragment, 960, 540);
    const float = (values: number[]) => ({ kind: "float" as const, values });
    renderer.frame({
      time: 1,
      delta: 0,
      index: 0,
      inputs: [
        { name: "night", value: float([0.06, 0.04, 0.16]) },
        { name: "violet", value: float([0.24, 0.11, 0.45]) },
        { name: "rose", value: float([0.84, 0.42, 0.47]) },
        { name: "teal", value: float([0.05, 0.36, 0.52]) },
        { name: "roseAt", value: float([0.5, 0.3]) },
        { name: "tealAt", value: float([1.3, 0.7]) },
        { name: "glowAt", value: float([0.9, 0.5]) },
      ],
    });
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    const gl = canvas.getContext("webgl2");
    if (!gl) throw new Error("no WebGL2");
    const pixel = new Uint8Array(4);
    gl.readPixels(480, 270, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
    return Array.from(pixel);
  });
  const [r = 0, g = 0, b = 0, a = 0] = center;
  expect(a).toBe(255);
  expect(r + g + b, `the colours the inputs set, not black: ${center}`).toBeGreaterThan(60);
});
