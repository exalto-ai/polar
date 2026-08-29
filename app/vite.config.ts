import { defineConfig } from "vite";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * Dev-only: serve the running daemon's connection details so the window can be
 * opened in a plain browser as well as under Tauri.
 *
 * Real builds get this from the Tauri `connection` command instead. The token
 * never goes in a URL either way.
 */
function thoughtConnection() {
  return {
    name: "thought-connection",
    configureServer(server: any) {
      server.middlewares.use("/__thought/connection", (_req: any, res: any) => {
        // Mirrors `support_dir` in crates/thoughtd/src/discovery.rs.
        const home =
          process.env.THOUGHT_HOME ??
          (process.platform === "darwin"
            ? join(homedir(), "Library/Application Support/ai.exalto.thought")
            : join(
                process.env.XDG_DATA_HOME ?? join(homedir(), ".local/share"),
                "thought",
              ));
        try {
          const config = JSON.parse(readFileSync(join(home, "daemon.json"), "utf8"));
          res.setHeader("Content-Type", "application/json");
          res.end(
            JSON.stringify({
              sync_url: config.url.replace("http://", "ws://").replace("/mcp", "/sync"),
              mcp_url: config.url,
              token: config.token,
              stdio_command: join(process.cwd(), "../target/debug/thought-mcp-stdio"),
              // Dev-only mirror of `thought_mcp::EDITOR_ACTOR_ID`; real builds
              // get it from the Tauri command, which reads the constant.
              actor_id: "human:editor",
            }),
          );
        } catch (error) {
          res.statusCode = 503;
          res.end(
            JSON.stringify({
              error: error instanceof Error ? error.message : "no daemon is running",
            }),
          );
        }
      });
    },
  };
}

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [thoughtConnection()],

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
