import { defineConfig } from "astro/config";

export default defineConfig({
  site: "https://danmu.elazer.wang",
  output: "static",
  build: {
    format: "directory",
  },
  vite: {
    build: {
      cssMinify: "lightningcss",
    },
  },
});
