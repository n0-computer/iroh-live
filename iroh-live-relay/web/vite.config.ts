import { resolve } from "path";
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";

export default defineConfig({
  root: "src",
  envDir: resolve(__dirname),
  plugins: [solidPlugin()],
  build: {
    target: "esnext",
    outDir: resolve(__dirname, "dist"),
    emptyOutDir: true,
    rollupOptions: {
      input: {
        watch: resolve(__dirname, "src/index.html"),
        publish: resolve(__dirname, "src/publish.html"),
      },
    },
  },
});
