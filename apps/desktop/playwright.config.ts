import { defineConfig } from "@playwright/test";

const PORT = 1421;

/** Runs the frontend against the in-memory backend (`--mode mock`) in the installed Edge, so no browser download. */
export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  reporter: "list",
  use: {
    baseURL: `http://localhost:${PORT}`,
    channel: "msedge",
    viewport: { width: 1440, height: 900 },
  },
  webServer: {
    command: `pnpm exec vite --mode mock --port ${PORT}`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: true,
  },
});
