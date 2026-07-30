import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  build: {
    outDir: ".build/web",
    emptyOutDir: true,
  },
});
