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
  await typeAtEnd(page, "script.");
  const suggestions = page.locator(".monaco-editor .suggest-widget");
  await expect(suggestions.getByText("on_draw")).toBeVisible();
  await expect(suggestions.getByText("every")).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(page.locator(".view-lines")).toContainText("script.every");
});

test("an edit that doesn't parse holds the last working preview and stays out of the log", async ({ page }) => {
  await typeAtEnd(page, "local broken = @@");
  await expect(page.getByText("Showing your last working version")).toBeVisible();
  await expect(page.getByRole("log")).not.toContainText("Incomplete statement");
  await expect(page.getByRole("img", { name: /Clock preview/ })).toBeVisible();
});

test("an error while running is marked at its line and logged once", async ({ page }) => {
  await typeAtEnd(page, "local x = nil.on_draw");
  await expect(page.getByText(/Stopped at line \d+/)).toBeVisible();
  await expect(page.locator(".monaco-editor .squiggly-error").first()).toBeVisible();
  await page.keyboard.type(" -- still broken", { delay: 40 });
  await page.waitForTimeout(1200);
  await expect(page.getByRole("log").getByText(/attempt to index nil/)).toHaveCount(1);
});
