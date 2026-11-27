import { expect, test, type Page } from "@playwright/test";

/** The editor theme's colours, as the browser reports them. */
const COLOURS = {
  keyword: "rgb(199, 155, 255)",
  type: "rgb(111, 211, 224)",
  annotation: "rgb(240, 198, 116)",
  comment: "rgb(111, 122, 139)",
  string: "rgb(159, 224, 168)",
  number: "rgb(245, 182, 120)",
  identifier: "rgb(214, 220, 230)",
} as const;

/** Waits for Monaco to draw the first token whose text is `text` in `colour`; it tokenizes after the first paint. */
async function expectColour(page: Page, text: string, colour: string) {
  const pattern = text.replace(/[[\]().*]/g, "\\$&").replace(/ /g, "\\s");
  const token = page.locator(".view-lines span span").filter({ hasText: new RegExp(`^${pattern}$`) }).first();
  await expect(token).toHaveCSS("color", colour);
}

test("Luau highlights keywords, annotations, strings and method calls", async ({ page }) => {
  await page.goto("/widgets/widget.local.user.clock");
  await expectColour(page, "local", COLOURS.keyword);
  await expectColour(page, "Options", COLOURS.type);
  await expectColour(page, "boolean", COLOURS.type);
  await expectColour(page, "Ui", COLOURS.type);
  await expectColour(page, "text", COLOURS.identifier);
  await expectColour(page, '"%H:%M:%S"', COLOURS.string);
  await expectColour(page, "0.5", COLOURS.number);
  await expectColour(page, "--[[ Draws the time ]]", COLOURS.comment);
});

test("Slang highlights attributes, vector types and suffixed numbers", async ({ page }) => {
  await page.goto("/backgrounds/background.local.user.aurora");
  await expectColour(page, "Canvas", COLOURS.annotation);
  await expectColour(page, "half4", COLOURS.type);
  await expectColour(page, "struct", COLOURS.keyword);
  await expectColour(page, "0.75h", COLOURS.number);
  await expectColour(page, "// Fade from night at the top to rose", COLOURS.comment);
});

test("each file tab says what it's for on hover", async ({ page }) => {
  await page.goto("/widgets/widget.local.user.clock");
  await page.getByRole("tab", { name: "host.luau" }).hover();
  await expect(page.getByRole("tooltip")).toHaveText("Runs on this device");
  await page.goto("/backgrounds/background.local.user.aurora");
  await page.getByRole("tab", { name: "canvas.slang" }).hover();
  await expect(page.getByRole("tooltip")).toHaveText("Shader");
});
