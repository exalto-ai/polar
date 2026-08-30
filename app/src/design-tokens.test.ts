import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const styles = readFileSync(resolve(import.meta.dirname, "styles.css"), "utf8");
const design = readFileSync(resolve(import.meta.dirname, "../../DESIGN.md"), "utf8");
const main = readFileSync(resolve(import.meta.dirname, "main.ts"), "utf8");
const names = readFileSync(resolve(import.meta.dirname, "names.ts"), "utf8");

const STATUS_TOKENS = {
  "--status-positive": "#76c392",
  "--status-positive-soft": "#9bd5ae",
  "--status-caution": "#d7a13a",
  "--status-caution-soft": "#e3c486",
  "--status-negative": "#d66b63",
  "--status-negative-soft": "#f0aaa4",
} as const;

describe("documented design tokens", () => {
  it("defines each semantic status colour once and documents its value", () => {
    for (const [token, value] of Object.entries(STATUS_TOKENS)) {
      expect(styles).toContain(`${token}: ${value};`);
      expect(styles.match(new RegExp(value, "g"))).toHaveLength(1);
      expect(design).toContain(`\`${value}\``);
    }
  });

  it("keeps the fallback actor colour in the presence palette module", () => {
    expect(names).toContain('FALLBACK_PRESENCE_COLOR = "#888"');
    expect(main).not.toContain('?? "#888"');
  });
});
