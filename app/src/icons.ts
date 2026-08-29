/**
 * Decorative editor icons from Lucide.
 *
 * The node data is copied from Lucide commit
 * 23f9abc4ed0146cffededd3d7f94c1018bfdf693. Keeping the small subset here
 * avoids adding a runtime dependency. See ../../THIRD_PARTY_NOTICES.md.
 */

const SVG_NAMESPACE = "http://www.w3.org/2000/svg";

type IconTag = "line" | "path";
type IconAttributes = Readonly<Record<string, string>>;
export type IconNode = readonly [tag: IconTag, attributes: IconAttributes];

export const ICONS = {
  link2: [
    ["path", { d: "M9 17H7A5 5 0 0 1 7 7h2" }],
    ["path", { d: "M15 7h2a5 5 0 1 1 0 10h-2" }],
    ["line", { x1: "8", x2: "16", y1: "12", y2: "12" }],
  ],
} as const satisfies Record<string, readonly IconNode[]>;

export function icon(nodes: readonly IconNode[]): SVGSVGElement {
  const svg = document.createElementNS(SVG_NAMESPACE, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");

  for (const [tag, attributes] of nodes) {
    const element = document.createElementNS(SVG_NAMESPACE, tag);
    for (const [name, value] of Object.entries(attributes)) {
      element.setAttribute(name, value);
    }
    svg.append(element);
  }

  return svg;
}
