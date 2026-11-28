import { expect, test, type Page } from "@playwright/test";

/** Loads the app with the mock keyboard running Flux software or Glassdeck. */
async function withKeyboard(page: Page, software: "flux" | "glassdeck", driver: "missing" | "ready" = "missing") {
  await page.addInitScript(
    ([s, d]) => {
      localStorage.setItem("glassdeck.mock.keyboard", s);
      localStorage.setItem("glassdeck.mock.driver", d);
    },
    [software, driver] as const,
  );
}

const section = (page: Page, name: string) => page.getByRole("region", { name, exact: true });

test("on Flux software, the page offers a backup and keeps Install locked", async ({ page }) => {
  await withKeyboard(page, "flux");
  await page.goto("/keyboard");
  await expect(page.getByRole("heading", { name: "Keyboard" })).toBeVisible();
  await expect(page.getByText("Serial E4645852431F3223")).toBeVisible();
  await expect(section(page, "Back up").getByRole("button", { name: "Back up" })).toBeEnabled();
  await expect(section(page, "Install").getByRole("button", { name: "Install Glassdeck" })).toBeDisabled();
  await expect(section(page, "Install").getByText("Back up first")).toBeVisible();
});

test("a backup stops for the driver, then finishes and unlocks Install", async ({ page }) => {
  await withKeyboard(page, "flux");
  await page.goto("/keyboard");
  const backUp = section(page, "Back up");
  await backUp.getByRole("button", { name: "Back up" }).click();
  await expect(page.getByText("Windows needs a driver to read the key scanner's memory")).toBeVisible();
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(backUp.getByText("Flux firmware 1.4.9.D, display 1.4.19, 149 MB")).toBeVisible();
  await expect(backUp.getByText("Keep a copy off this PC. The factory calibration can't be made again.")).toBeVisible();
  await expect(backUp.getByText("Firmware, settings and factory calibration")).toBeVisible();
  await expect(backUp.getByRole("button", { name: "Backed up" })).toBeDisabled();
  const install = section(page, "Install");
  await expect(install.getByRole("button", { name: "Install Glassdeck" })).toBeEnabled();
  await install.getByRole("button", { name: "Install Glassdeck" }).click();
  await expect(install.getByRole("alert")).toContainText("Installing isn't ready yet");
});

test("on Glassdeck, the page shows the keyboard and asks before restoring", async ({ page }) => {
  await withKeyboard(page, "glassdeck");
  await page.goto("/keyboard");
  await expect(section(page, "Key scanner").getByText("glassdeck-scanner 0.0.1")).toBeVisible();
  await expect(section(page, "Keyboard's computer").getByText("Flux OS 0.0.9")).toBeVisible();
  await expect(page.getByRole("button", { name: "Back up" })).toHaveCount(0);
  const restore = section(page, "Restore Flux software");
  await expect(restore.getByText("Back up first")).toBeVisible();
  await expect(restore.getByRole("button", { name: "Restore", exact: true })).toBeDisabled();
  await section(page, "Backups").getByRole("button", { name: "Import folder" }).click();
  await expect(section(page, "Backups").getByText("Flux firmware 1.4.9.D, display 1.4.19, 149 MB")).toBeVisible();
  await expect(restore.getByText(/From the backup of/)).toBeVisible();
  await restore.getByRole("button", { name: "Restore", exact: true }).click();
  await expect(restore.getByRole("button", { name: "Restore now" })).toBeVisible();
  await restore.getByRole("button", { name: "Cancel" }).click();
  await expect(restore.getByRole("button", { name: "Restore now" })).toHaveCount(0);
  await restore.getByRole("button", { name: "Restore", exact: true }).click();
  await restore.getByRole("button", { name: "Restore now" }).click();
  await expect(restore.getByRole("alert")).toContainText("checked out, but restoring isn't ready yet");
  await expect(page.getByRole("img", { name: "Needs setup" })).toHaveCount(0);
});

test("importing a backup made elsewhere unlocks Install", async ({ page }) => {
  await withKeyboard(page, "flux");
  await page.goto("/keyboard");
  const backUp = section(page, "Back up");
  await expect(backUp.getByText("None yet")).toBeVisible();
  await backUp.getByRole("button", { name: "Import zip" }).click();
  await expect(backUp.getByText("Flux firmware 1.4.9.D, display 1.4.19, 149 MB")).toBeVisible();
  await expect(backUp.getByRole("button", { name: "Show in folder" })).toBeVisible();
  await expect(backUp.getByRole("button", { name: "Export" })).toBeVisible();
  await expect(section(page, "Install").getByRole("button", { name: "Install Glassdeck" })).toBeEnabled();
});

test("on Flux software, the sidebar flags the keyboard and Overview can't apply", async ({ page }) => {
  await withKeyboard(page, "flux");
  await page.goto("/");
  const nav = page.getByRole("navigation", { name: "Main" });
  await expect(nav.getByRole("link", { name: /Keyboard/ }).getByRole("img", { name: "Needs setup" })).toBeVisible();
  await expect(nav.getByText("Keyboard needs setup")).toBeVisible();
  await expect(page.getByRole("button", { name: "Apply to keyboard" })).toBeDisabled();
  await page.getByRole("link", { name: "Set up the keyboard to apply" }).click();
  await expect(page.getByRole("heading", { name: "Keyboard" })).toBeVisible();
});
