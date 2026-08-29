import { describe, expect, it } from "vitest";
import { ICONS, icon } from "./icons";

describe("Lucide icon rendering", () => {
  it("keeps decorative SVGs out of the accessibility tree", () => {
    const svg = icon(ICONS.link2);

    expect(svg.getAttribute("viewBox")).toBe("0 0 24 24");
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    expect(svg.getAttribute("focusable")).toBe("false");
  });

  it("preserves the mixed SVG primitives in the official icon data", () => {
    expect([...icon(ICONS.link2).children].map(({ tagName }) => tagName)).toEqual([
      "path",
      "path",
      "line",
    ]);
  });
});
