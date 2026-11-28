import { expect, test } from "@playwright/test";
import { applyState } from "../src/applying";

test("Apply to keyboard waits for setup, then stays off only while the keyboard has this exact background", () => {
  const upToDate = { needsSetup: false, onKeyboard: true, unsaved: false, saves: 2, applied: 2 };
  expect(applyState(upToDate)).toBe("on keyboard");
  expect(applyState({ ...upToDate, needsSetup: true })).toBe("set up");
  expect(applyState({ ...upToDate, onKeyboard: false })).toBe("ready");
  expect(applyState({ ...upToDate, unsaved: true })).toBe("ready");
  expect(applyState({ ...upToDate, saves: 3 })).toBe("ready");
});

test("a background that isn't on the keyboard applies, then reads On the keyboard until it's edited", async ({ page }) => {
  await page.goto("/backgrounds/background.local.user.aurora");
  const apply = page.getByRole("button", { name: "Apply to keyboard" });
  await expect(apply).toBeEnabled();
  await apply.click();
  await expect(page.getByRole("button", { name: "On the keyboard" })).toBeDisabled();

  await page.locator(".monaco-editor .view-lines").click();
  await page.keyboard.press("Control+End");
  await page.keyboard.type("\n// warmer");
  await expect(page.getByRole("button", { name: "Apply to keyboard" })).toBeEnabled();
  await page.getByRole("button", { name: "Apply to keyboard" }).click();
  await expect(page.getByRole("button", { name: "Saved" })).toBeVisible();
  await expect(page.getByRole("button", { name: "On the keyboard" })).toBeDisabled();
});

test("on Flux software, Apply to keyboard waits for setup", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("glassdeck.mock.keyboard", "flux"));
  await page.goto("/backgrounds/background.local.user.mountain-dawn");
  await expect(page.getByRole("link", { name: "Set up the keyboard to apply" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply to keyboard" })).toBeDisabled();
});
