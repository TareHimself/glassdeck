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
    renderer.useProgram({ fragment, width: 960, height: 540, passes: [], targets: [] });
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

/** The audio spectrum's real shader (e2e/fixtures/spectrum.essl), whose levels are an array input. */
test("an array input reaches the shader", async ({ page }) => {
  await page.goto("/");
  const [low, high] = await page.evaluate(async () => {
    const { CanvasRenderer } = await import("/src/canvasRenderer.ts");
    const fragment: string = (await import("/e2e/fixtures/spectrum.essl?raw")).default;
    const canvas = document.createElement("canvas");
    document.body.append(canvas);
    const renderer = new CanvasRenderer(canvas);
    renderer.useProgram({ fragment, width: 960, height: 540, passes: [], targets: [] });
    const levels = Array.from({ length: 24 }, (_, i) => (i < 12 ? 0.9 : 0));
    renderer.frame({ time: 1, delta: 0, index: 0, inputs: [{ name: "levels", value: { kind: "array", values: levels } }] });
    await new Promise(requestAnimationFrame);
    await new Promise(requestAnimationFrame);
    const gl = canvas.getContext("webgl2");
    if (!gl) throw new Error("no WebGL2");
    const brightness = (x: number) => {
      const pixel = new Uint8Array(4);
      gl.readPixels(x, 135, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
      return pixel[0]! + pixel[1]! + pixel[2]!;
    };
    return [brightness(100), brightness(860)];
  });
  expect(low, "a band set to 0.9 draws a bar").toBeGreaterThan(200);
  expect(high, "a silent band draws none").toBeLessThan(80);
});

/** Flow's real shaders (e2e/fixtures/flow-pass.essl and flow.essl): its pass reads its own last frame. */
test("a feedback target keeps last frame's picture", async ({ page }) => {
  await page.goto("/");
  const [after1, after20] = await page.evaluate(async () => {
    const { CanvasRenderer } = await import("/src/canvasRenderer.ts");
    const pass: string = (await import("/e2e/fixtures/flow-pass.essl?raw")).default;
    const fragment: string = (await import("/e2e/fixtures/flow.essl?raw")).default;
    const canvas = document.createElement("canvas");
    document.body.append(canvas);
    const renderer = new CanvasRenderer(canvas);
    renderer.useProgram({
      fragment,
      width: 480,
      height: 270,
      passes: [{ fragment: pass, writes: "ink" }],
      targets: [{ name: "ink", width: 480, height: 270, format: "rgba16f", feedback: true }],
    });
    const one = (name: string, value: number) => ({ name, value: { kind: "float" as const, values: [value] } });
    const inputs = [one("bass", 0.5), one("mid", 0.8), one("treble", 0), one("beat", 0), one("beatAge", 10), one("phase", 0), one("hue", 0.2)];
    const gl = canvas.getContext("webgl2");
    if (!gl) throw new Error("no WebGL2");
    const brightness = () => {
      const pixels = new Uint8Array(480 * 270 * 4);
      gl.readPixels(0, 0, 480, 270, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      return pixels.reduce((sum, v, i) => (i % 4 === 3 ? sum : sum + v), 0);
    };
    const results: number[] = [];
    for (let index = 0; index < 20; index++) {
      renderer.frame({ time: index / 30, delta: 1 / 30, index, inputs: index === 0 ? inputs : [] });
      await new Promise(requestAnimationFrame);
      await new Promise(requestAnimationFrame);
      if (index === 0 || index === 19) results.push(brightness());
    }
    return results;
  });
  // Every frame gets the same inputs, so without the history frame 20 would match frame 1 exactly.
  expect(after20, `ink builds up over frames: ${after1} then ${after20}`).toBeGreaterThan((after1 ?? 0) * 1.1);
});
