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

test("Change background searches the library and picks into the draft", async ({ page }) => {
  await backgroundCard(page).getByRole("button", { name: "Change" }).click();
  const dialog = page.getByRole("dialog", { name: "Change background" });
  await expect(dialog.getByLabel("Search backgrounds")).toBeFocused();
  await page.keyboard.type("aur");
  await expect(dialog.getByRole("button", { name: /Rain on glass/ })).toHaveCount(0);
  await dialog.getByRole("button", { name: /Aurora/ }).click();
  await expect(dialog).toBeHidden();
  await expect(backgroundCard(page).getByText("Aurora")).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply to keyboard" })).toBeEnabled();
});

test("dialogs close on Escape and on a click outside", async ({ page }) => {
  const dialog = page.getByRole("dialog", { name: "Change background" });
  await backgroundCard(page).getByRole("button", { name: "Change" }).click();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await backgroundCard(page).getByRole("button", { name: "Change" }).click();
  await page.mouse.click(5, 5);
  await expect(dialog).toBeHidden();
});

test("Add widget places it in a free spot and selects it", async ({ page }) => {
  await widgetsCard(page).getByRole("button", { name: "Add widget" }).click();
  await page.getByRole("dialog", { name: "Add widget" }).getByRole("button", { name: /Now playing/ }).click();
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
  await backgroundCard(page).getByRole("button", { name: "Change" }).click();
  await page.getByRole("dialog").getByRole("button", { name: /Aurora/ }).click();
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
