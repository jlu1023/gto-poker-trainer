import { defineConfig } from "vite";

// base "./" so the built static site works when opened locally from any path
export default defineConfig({
  base: "./",
  build: {
    target: "es2022",
    outDir: "dist",
  },
});
