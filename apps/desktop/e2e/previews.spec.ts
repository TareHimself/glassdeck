import { expect, test } from "@playwright/test";
import { HOVER_PLAY_DELAY_MS } from "../src/components/hoverPlay";

test("a late shader from the previous preview never shows on the next", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Change" }).click();
  const dialog = page.getByRole("dialog", { name: "Change background" });
  await dialog.getByRole("button", { name: /Aurora/ }).hover();
  await page.waitForTimeout(300);
  // Each switch makes the mock send an all-black shader from the run it replaced, as a slow real one would arrive
  const gradient = dialog.getByRole("button", { name: /Gradient/ });
  await gradient.hover();
  const canvas = gradient.locator("canvas");
  await expect(canvas).toHaveCount(1);
  const sawBlack = await canvas.evaluate(
    (element) =>
      new Promise<boolean>((resolve) => {
        const live = element as HTMLCanvasElement;
        const gl = live.getContext("webgl2");
        if (!gl) return resolve(true);
        let black = false;
        const until = performance.now() + 900;
        const sample = () => {
          const pixel = new Uint8Array(4);
          gl.readPixels(live.width >> 1, live.height >> 1, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, pixel);
          if (pixel[3] === 255 && pixel[0] === 0 && pixel[1] === 0 && pixel[2] === 0) black = true;
          if (performance.now() < until) requestAnimationFrame(sample);
          else resolve(black);
        };
        sample();
      }),
  );
  expect(sawBlack).toBe(false);
});

test("cards ask for a missing still and show it", async ({ page }) => {
  await page.goto("/widgets");
  for (const title of ["Clock", "Now playing", "Pomodoro"]) {
    await expect(page.getByRole("img", { name: title })).toBeVisible();
  }
});

test("library cards play once hovered for a moment, and stop when left", async ({ page }) => {
  for (const [route, name, live, still] of [
    ["/backgrounds", /Aurora/, "canvas", 0],
    ["/widgets", /Now playing/, "img", 1],
  ] as const) {
    await page.goto(route);
    const card = page.getByRole("link", { name });
    await expect(card.locator(live), `${route}: its still`).toHaveCount(still);
    const before = still;
    await card.hover();
    await page.waitForTimeout(HOVER_PLAY_DELAY_MS / 3);
    await expect(card.locator(live), `${route}: not yet`).toHaveCount(before);
    await expect.poll(() => card.locator(live).count(), { message: `${route}: playing` }).toBeGreaterThan(before);
    await page.mouse.move(0, 0);
    await expect(card.locator(live), `${route}: stopped`).toHaveCount(before);
  }
});

test("hovering a card plays it over its still", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Change" }).click();
  const aurora = page.getByRole("dialog", { name: "Change background" }).getByRole("button", { name: /Aurora/ });
  await expect(aurora.locator("img")).toHaveCount(1);
  await aurora.hover();
  await expect(aurora.locator("canvas")).toBeVisible();
  await expect(aurora.locator("img")).toHaveCount(1);
});

test("Use this frame makes the moment on screen the canvas's still", async ({ page }) => {
  await page.goto("/backgrounds/background.local.user.aurora");
  const use = page.getByRole("button", { name: "Use this frame" });
  await expect(use).toBeEnabled();
  await use.click();
  await expect(page.getByRole("button", { name: "Preview set" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Use this frame" })).toBeVisible({ timeout: 3000 });
});
