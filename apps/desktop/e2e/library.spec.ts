import { expect, test } from "@playwright/test";

test("library cards show titles, not package ids", async ({ page }) => {
  await page.goto("/backgrounds");
  await expect(page.getByText("Aurora")).toBeVisible();
  await expect(page.getByText("background.local.user.aurora")).toHaveCount(0);
});

test("Rename from a card's menu focuses the title field", async ({ page }) => {
  await page.goto("/widgets");
  await page.getByRole("button", { name: "More actions for Clock" }).click();
  await page.getByRole("menuitem", { name: "Rename" }).click();
  await expect(page.getByLabel("Title")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Title")).toHaveCount(0);
});

test("the background filter narrows the library", async ({ page }) => {
  await page.goto("/backgrounds");
  await page.getByRole("radio", { name: "Video" }).click();
  await expect(page.getByText("Rain on glass")).toBeVisible();
  await expect(page.getByText("Aurora")).toHaveCount(0);
});

test("an image background is made from a picture, and the Image filter finds it", async ({ page }) => {
  await page.goto("/backgrounds");
  await page.getByRole("radio", { name: "Image" }).click();
  await expect(page.getByRole("link", { name: /Mountain dawn/ })).toBeVisible();
  await expect(page.getByRole("link", { name: /Aurora/ })).toHaveCount(0);

  await page.getByRole("button", { name: "New background" }).first().click();
  const dialog = page.getByRole("dialog", { name: "New background" });
  await dialog.getByRole("radio", { name: "Image" }).click();
  await expect(dialog.getByRole("button", { name: "Create" })).toBeDisabled();
  await dialog.getByRole("button", { name: "Choose" }).click();
  await expect(dialog.getByRole("status")).toHaveText("Fitting the picture to the keyboard");
  await expect(dialog.getByRole("button", { name: "Loading…" })).toBeDisabled();
  await expect(dialog.getByRole("img", { name: "How the picture fills the keyboard" })).toBeVisible();
  await expect(dialog.getByRole("status")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "Choose" })).toBeEnabled();
  await expect(dialog.getByLabel("Title")).toHaveValue("mountain-dawn");
  await dialog.getByLabel("Title").fill("Peaks");
  await dialog.getByRole("button", { name: "Create" }).click();

  await expect(page.getByRole("heading", { name: "Peaks" })).toBeVisible();
  await expect(page.getByText("image background")).toBeVisible();
  await expect(page.getByText("1920 × 1080")).toBeVisible();
  await expect(page.getByText("mountain-dawn.jpg")).toBeVisible();
});
