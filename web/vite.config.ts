import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const apiTarget = process.env.VITE_API_TARGET ?? "http://127.0.0.1:8080";
const port = Number(process.env.VITE_PORT ?? 5173);

export default defineConfig({
  plugins: [react()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    host: "127.0.0.1",
    port,
    strictPort: true,
    proxy: {
      "/api": { target: apiTarget, changeOrigin: true, ws: true },
      "/plugin-ui": { target: apiTarget, changeOrigin: true },
      "/plugin-ui-version": { target: apiTarget, changeOrigin: true },
      "/plugin-ui-access": { target: apiTarget, changeOrigin: true },
      "/plugin-icon": { target: apiTarget, changeOrigin: true },
    },
  },
});
