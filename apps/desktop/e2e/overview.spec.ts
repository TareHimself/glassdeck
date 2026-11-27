import { expect, test, type Page } from "@playwright/test";

const backgroundCard = (page: Page) => page.locator("section", { has: page.getByRole("heading", { name: "Background", exact: true }) });
const widgetsCard = (page: Page) => page.locator("section", { has: page.getByRole("heading", { name: "Widgets", exact: true }) });

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await expect(widgetsCard(page).getByRole("button", { name: /Clock/ })).toBeVisible();
});

test("shows the active theme and a connected keyboard", async ({ page }) => {
  await expect(backgroundCard(page).getByText("Gradient")).toBeVisible();
  await expect(page.getByText("Keyboard connected")).toBeVisible();
  await expect(page.getByRole("button", { name: "Keyboard up to date" })).toBeDisabled();
});

test("Change opens the backgrounds in select mode, and a pick goes into the draft", async ({ page }) => {
  await backgroundCard(page).getByRole("link", { name: "Change" }).click();
  await expect(page.getByRole("heading", { name: "Choose a background" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Main" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Gradient/ })).toContainText("Current");
  await page.getByLabel("Search backgrounds").fill("aur");
  await expect(page.getByRole("button", { name: /Rain on glass/ })).toHaveCount(0);
  await page.getByRole("button", { name: /Aurora/ }).click();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();
  await expect(backgroundCard(page).getByText("Aurora")).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply to keyboard" })).toBeEnabled();
});

test("going back from select mode changes nothing", async ({ page }) => {
  await backgroundCard(page).getByRole("link", { name: "Change" }).click();
  await page.getByRole("link", { name: "← Overview" }).click();
  await expect(backgroundCard(page).getByText("Gradient")).toBeVisible();
  await expect(page.getByRole("button", { name: "Keyboard up to date" })).toBeDisabled();
});

test("Add widget places it in a free spot and selects it", async ({ page }) => {
  await widgetsCard(page).getByRole("link", { name: "Add widget" }).click();
  await expect(page.getByRole("heading", { name: "Add a widget" })).toBeVisible();
  await expect(page.getByRole("button", { name: /Clock.*On the keyboard/ })).toBeVisible();
  await page.getByRole("button", { name: /Now playing/ }).click();
  const row = widgetsCard(page).getByRole("button", { name: /Now playing/ });
  await expect(row).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("heading", { name: "Now playing", level: 2 })).toBeVisible();
  await expect(page.getByText("This widget has no options.")).toBeVisible();
});

test("a selected widget shows its options", async ({ page }) => {
  await widgetsCard(page).getByRole("button", { name: /Clock/ }).click();
  await expect(page.getByLabel("Show seconds")).toBeChecked();
});

test("dragging a widget moves it on the 8 px grid", async ({ page }) => {
  const tile = page.getByRole("button", { name: "Select Clock" });
  const box = await tile.boundingBox();
  if (!box) throw new Error("the Clock tile isn't laid out");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 150, box.y + box.height / 2, { steps: 8 });
  await page.mouse.up();
  const geometry = await widgetsCard(page).getByRole("button", { name: /Clock/ }).innerText();
  const [x] = geometry.match(/(\d+), \d+/)?.slice(1).map(Number) ?? [];
  expect(x).toBeGreaterThan(0);
  expect((x ?? 1) % 8).toBe(0);
});

test("an outdated keyboard is named, and Apply says to update it", async ({ page }) => {
  await page.evaluate(() => window.glassdeckMock?.setKeyboard({ state: "incompatible", keyboard: 11, host: 12 }));
  await expect(page.getByText("Keyboard needs an update")).toBeVisible();
  await backgroundCard(page).getByRole("link", { name: "Change" }).click();
  await page.getByRole("button", { name: /Aurora/ }).click();
  await page.getByRole("button", { name: "Apply to keyboard" }).click();
  await expect(page.getByText(/Update the keyboard/)).toBeVisible();
});

test("the keyboard address stays hidden until asked for", async ({ page }) => {
  await expect(page.getByText("10.0.0.186:7878")).toHaveCount(0);
  await page.getByRole("button", { name: "Show address" }).click();
  await expect(page.getByText("10.0.0.186:7878")).toBeVisible();
  await page.getByRole("button", { name: "Hide address" }).click();
  await expect(page.getByText("10.0.0.186:7878")).toHaveCount(0);
});

test("Live shows only for a chosen stream, green while it's attached", async ({ page }) => {
  const streamCard = page.locator("section", { has: page.getByRole("heading", { name: "Stream", exact: true }) });
  const pill = streamCard.getByText("Live");
  await expect(pill).toBeHidden();
  const heightWithNone = (await streamCard.boundingBox())?.height;
  await streamCard.getByRole("combobox", { name: "Stream" }).click();
  await page.getByRole("option", { name: "monitor-1" }).click();
  await expect(pill).toHaveAttribute("data-live", "true");
  await expect(pill).toBeVisible();
  expect((await streamCard.boundingBox())?.height, "the card doesn't grow when the pill appears").toBe(heightWithNone);
});

test("an attached stream shows in the keyboard preview instead of the background", async ({ page }) => {
  const streamCard = page.locator("section", { has: page.getByRole("heading", { name: "Stream", exact: true }) });
  await expect(page.getByRole("img", { name: "monitor-1 now" })).toHaveCount(0);
  await streamCard.getByRole("combobox", { name: "Stream" }).click();
  await page.getByRole("option", { name: "monitor-1" }).click();
  await expect(page.getByRole("img", { name: "monitor-1 now" })).toBeVisible();
  await streamCard.getByRole("combobox", { name: "Stream" }).click();
  await page.getByRole("option", { name: "None" }).click();
  await expect(page.getByRole("img", { name: "monitor-1 now" })).toHaveCount(0);
});

test("an attached stream plays live over WebRTC and the session ends with it", async ({ page }) => {
  const streamCard = page.locator("section", { has: page.getByRole("heading", { name: "Stream", exact: true }) });
  await streamCard.getByRole("combobox", { name: "Stream" }).click();
  await page.getByRole("option", { name: "monitor-1" }).click();
  const video = page.getByLabel("monitor-1 live");
  await expect(video).toBeVisible({ timeout: 10_000 });
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.videoWidth)).toBe(480);
  expect(await page.evaluate(() => window.glassdeckMock?.watching())).toBe(1);
  await streamCard.getByRole("combobox", { name: "Stream" }).click();
  await page.getByRole("option", { name: "None" }).click();
  await expect(video).toHaveCount(0);
  await expect.poll(() => page.evaluate(() => window.glassdeckMock?.watching())).toBe(0);
});

test("an editor opened from the Overview goes back to the Overview", async ({ page }) => {
  await page.getByRole("link", { name: "Change" }).click();
  await page.getByRole("button", { name: /Aurora/ }).click();
  await page.getByRole("link", { name: "Edit" }).first().click();
  await page.getByRole("link", { name: "← Overview" }).click();
  await expect(page.getByRole("heading", { name: "Overview" })).toBeVisible();

  await page.goto("/backgrounds/background.local.user.aurora");
  await expect(page.getByRole("link", { name: "← Backgrounds" })).toBeVisible();
});
