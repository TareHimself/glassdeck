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
