import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri drives the dev server on a fixed port and expects a stable origin, so
// `strictPort` is on: silently sliding to 1421 would leave the webview pointed
// at a dead URL.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // The Rust side is rebuilt by cargo, not vite; watching it wastes cycles
      // and retriggers HMR on every incremental build artifact.
      ignored: ["**/src-tauri/**"],
    },
  },
});
