import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
export default defineConfig({
  plugins: [vue()],
  base: "./",
  server: {
    proxy: {
      "/api": {
        target: process.env.CRONHARBOR_API_URL ?? "http://127.0.0.1:8090",
        changeOrigin: false,
      },
    },
  },
  build: { sourcemap: false },
});
