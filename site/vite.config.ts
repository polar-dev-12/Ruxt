import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const root = fileURLToPath(new URL(".", import.meta.url));

// Served from https://<user>.github.io/ruxt/ by the Pages workflow; override with
// VITE_BASE=/ for a custom domain.
export default defineConfig({
  base: process.env.VITE_BASE ?? "/ruxt/",
  plugins: [react()],
  build: {
    target: "es2020",
    sourcemap: false,
    rollupOptions: {
      input: {
        main: resolve(root, "index.html"),
        docs: resolve(root, "docs.html"),
      },
    },
  },
});
