import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const projectRoot = resolve(import.meta.dirname, "../..");
const tauriConfig = JSON.parse(
  readFileSync(resolve(projectRoot, "app/src-tauri/tauri.conf.json"), "utf8"),
) as {
  bundle?: { resources?: Record<string, string> };
};
const notices = readFileSync(resolve(projectRoot, "THIRD_PARTY_NOTICES.md"), "utf8");

describe("third-party notices", () => {
  it("packages the repository notice at a stable app resource path", () => {
    expect(tauriConfig.bundle?.resources?.["../../THIRD_PARTY_NOTICES.md"]).toBe(
      "THIRD_PARTY_NOTICES.md",
    );
  });

  it("retains the copied Lucide and Feather attribution", () => {
    expect(notices).toContain("23f9abc4ed0146cffededd3d7f94c1018bfdf693");
    expect(notices).toContain("Copyright (c) 2026 Lucide Icons and Contributors");
    expect(notices).toContain("Copyright (c) 2013-present Cole Bemis");
  });
});
