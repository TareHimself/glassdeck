import { expect, test, type Page } from "@playwright/test";

/** Puts the cursor at the end of the open script, ready to type a new line. */
async function typeAtEnd(page: Page, text: string) {
  await page.locator(".monaco-editor .view-lines").click();
  await page.keyboard.press("Control+End");
  await page.keyboard.press("Enter");
  // At a person's pace: Monaco opens completion mid-word, and faster keys race it
  await page.keyboard.type(text, { delay: 40 });
}

test.beforeEach(async ({ page }) => {
  await page.goto("/widgets/widget.local.user.clock");
  await expect(page.getByText(/no problems/)).toBeVisible();
});

test("errors show as you type, before saving", async ({ page }) => {
  await typeAtEnd(page, 'local count: number = "three"');
  await expect(page.locator(".monaco-editor .squiggly-error")).toBeVisible();
  await page.locator(".view-lines span span").filter({ hasText: /^"three"$/ }).hover();
  await expect(page.getByText("Type 'string' could not be converted into 'number'")).toBeVisible();
});

test("typing a member access offers the server's completions", async ({ page }) => {
  await typeAtEnd(page, "widget.");
  const suggestions = page.locator(".monaco-editor .suggest-widget");
  await expect(suggestions.getByText("on_draw")).toBeVisible();
  await expect(suggestions.getByText("every")).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(page.locator(".view-lines")).toContainText("widget.every");
});
