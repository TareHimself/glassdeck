import { expect, test } from "@playwright/test";

declare global {
  interface Window {
    shownImages?: Set<string>;
  }
}

test("a late frame from the previous preview never shows on the next", async ({ page }) => {
  await page.goto("/");
  // Every image the page shows, however briefly: a stale frame flashes for only one frame interval
  await page.evaluate(() => {
    const shown = new Set<string>();
    window.shownImages = shown;
    const record = (node: Node) => {
      if (node instanceof HTMLImageElement) shown.add(node.src);
      if (node instanceof Element) node.querySelectorAll("img").forEach((image) => shown.add(image.src));
    };
    new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        if (mutation.type === "attributes") record(mutation.target);
        mutation.addedNodes.forEach(record);
      }
    }).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["src"] });
  });
  await page.getByRole("button", { name: "Change" }).click();
  const dialog = page.getByRole("dialog", { name: "Change background" });
  await dialog.getByRole("button", { name: /Aurora/ }).hover();
  await page.waitForTimeout(300);
  // Each switch makes the mock send a black frame from the run it replaced, as a slow real one would arrive
  await dialog.getByRole("button", { name: /Gradient/ }).hover();
  await page.waitForTimeout(800);
  const stale = await page.evaluate(() => window.glassdeckMock?.staleFrame ?? "");
  expect(stale).not.toBe("");
  const shown = await page.evaluate(() => [...(window.shownImages ?? [])]);
  expect(shown.length).toBeGreaterThan(0);
  expect(shown.filter((src) => src.includes(stale))).toEqual([]);
});

test("cards ask for a missing still and show it", async ({ page }) => {
  await page.goto("/widgets");
  for (const title of ["Clock", "Now playing", "Pomodoro"]) {
    await expect(page.getByRole("img", { name: title })).toBeVisible();
  }
});

test("hovering a card plays it over its still", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Change" }).click();
  const aurora = page.getByRole("dialog", { name: "Change background" }).getByRole("button", { name: /Aurora/ });
  await expect(aurora.locator("img")).toHaveCount(1);
  await aurora.hover();
  await expect(aurora.locator("img")).toHaveCount(2);
  const [still, live] = await aurora.locator("img").evaluateAll((images) => images.map((image) => (image as HTMLImageElement).src));
  expect(still).not.toBe(live);
});

test("Use this frame makes the moment on screen the canvas's still", async ({ page }) => {
  await page.goto("/backgrounds/background.local.user.aurora");
  const use = page.getByRole("button", { name: "Use this frame" });
  await expect(use).toBeEnabled();
  await use.click();
  await expect(page.getByRole("button", { name: "Preview set" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Use this frame" })).toBeVisible({ timeout: 3000 });
});
