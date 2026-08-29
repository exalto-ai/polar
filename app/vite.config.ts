import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          // These are stable vendor boundaries, not lazy product boundaries.
          // The browser still loads every entry at startup, while a small app
          // change no longer rewrites one large JavaScript asset.
          if (
            id.includes("/node_modules/@tiptap/pm/") ||
            id.includes("/node_modules/prosemirror-")
          ) {
            return "prosemirror-vendor";
          }
          if (id.includes("/node_modules/@tiptap/")) {
            return "tiptap-vendor";
          }
          if (
            id.includes("/node_modules/yjs/") ||
            id.includes("/node_modules/y-protocols/") ||
            id.includes("/node_modules/lib0/")
          ) {
            return "collaboration-vendor";
          }
        },
      },
    },
  },
  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
