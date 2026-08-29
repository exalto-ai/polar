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
    expect([...icon(ICONS.globe).children].map(({ tagName }) => tagName)).toEqual([
      "circle",
      "path",
      "path",
    ]);
    expect([...icon(ICONS.copy).children].map(({ tagName }) => tagName)).toEqual([
      "rect",
      "path",
    ]);
    expect([...icon(ICONS.link2Off).children].map(({ tagName }) => tagName)).toEqual([
      "path",
      "path",
      "line",
      "line",
    ]);
  });
});
