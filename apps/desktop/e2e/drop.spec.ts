import { expect, test } from "@playwright/test";

const drag = (page: import("@playwright/test").Page, type: "enter" | "drop" | "leave", paths: string[] = []) =>
  page.evaluate(([type, paths]) => window.glassdeckMock?.dragFiles(type as "enter" | "drop" | "leave", paths as string[]), [type, paths] as const);

test("dropping one image on the library opens New background with it chosen", async ({ page }) => {
  await page.goto("/backgrounds");
  await expect(page.getByRole("heading", { name: "Backgrounds" })).toBeVisible();
  await drag(page, "enter", ["C:\\Pictures\\mountain-dawn.jpg"]);
  await expect(page.getByText("Drop to add an image background")).toBeVisible();
  await expect(page.getByText("mountain-dawn.jpg")).toBeVisible();
  await drag(page, "drop", ["C:\\Pictures\\mountain-dawn.jpg"]);
  await expect(page.getByText("Drop to add an image background")).toBeHidden();
  const dialog = page.getByRole("dialog", { name: "New background" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("radio", { name: "Image" })).toBeChecked();
  await expect(dialog.getByText("C:\\Pictures\\mountain-dawn.jpg")).toBeVisible();
});

test("a video opens as a video, and several files or other kinds are refused", async ({ page }) => {
  await page.goto("/backgrounds");
  await expect(page.getByRole("heading", { name: "Backgrounds" })).toBeVisible();
  await drag(page, "enter", ["C:\\a.jpg", "C:\\b.jpg"]);
  await expect(page.getByText("One image or video at a time")).toBeVisible();
  await drag(page, "leave");
  await expect(page.getByText("One image or video at a time")).toBeHidden();
  await drag(page, "enter", ["C:\\notes.txt"]);
  await expect(page.getByText("One image or video at a time")).toBeVisible();
  await drag(page, "drop", ["C:\\notes.txt"]);
  await expect(page.getByRole("dialog")).toBeHidden();

  await drag(page, "enter", ["D:\\Clips\\rain.MP4"]);
  await expect(page.getByText("Drop to add a video background")).toBeVisible();
  await drag(page, "drop", ["D:\\Clips\\rain.MP4"]);
  const dialog = page.getByRole("dialog", { name: "New background" });
  await expect(dialog.getByRole("radio", { name: "Video" })).toBeChecked();
  await expect(dialog.getByText("D:\\Clips\\rain.MP4")).toBeVisible();
});
