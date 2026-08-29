import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const mainSource = readFileSync(resolve(import.meta.dirname, "main.ts"), "utf8");
const viteSource = readFileSync(
  resolve(import.meta.dirname, "../vite.config.ts"),
  "utf8",
);

describe("native daemon bootstrap boundary", () => {
  it("does not expose daemon capabilities through the development web server", () => {
    expect(viteSource).not.toContain("daemon.json");
    expect(viteSource).not.toContain("/__thought/connection");
    expect(viteSource).not.toContain("mcp_token");
    expect(viteSource).not.toContain("editor_token");
  });

  it("loads connection capabilities only through the native command", () => {
    expect(mainSource).toContain('invoke<Connection>("connection")');
    expect(mainSource).not.toContain('fetch("/__thought/connection")');
  });
});
