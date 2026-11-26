import react from "@vitejs/plugin-react";
import wyw from "@wyw-in-js/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), wyw({ include: ["src/**/*.{ts,tsx}"] })],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "chrome120" },
});
