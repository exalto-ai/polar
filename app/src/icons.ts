/**
 * Decorative editor icons from Lucide.
 *
 * The node data is copied from Lucide commit
 * 23f9abc4ed0146cffededd3d7f94c1018bfdf693. Keeping the small subset here
 * avoids adding a runtime dependency. See ../../THIRD_PARTY_NOTICES.md.
 */

const SVG_NAMESPACE = "http://www.w3.org/2000/svg";

type IconTag = "circle" | "line" | "path" | "rect";
type IconAttributes = Readonly<Record<string, string>>;
export type IconNode = readonly [tag: IconTag, attributes: IconAttributes];

export const ICONS = {
  filePlus: [
    [
      "path",
      {
        d: "M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z",
      },
    ],
    ["path", { d: "M14 2v5a1 1 0 0 0 1 1h5" }],
    ["path", { d: "M9 15h6" }],
    ["path", { d: "M12 18v-6" }],
  ],
  folderOpen: [
    [
      "path",
      {
        d: "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2",
      },
    ],
  ],
  save: [
    [
      "path",
      {
        d: "M15.2 3a2 2 0 0 1 1.4.6l3.8 3.8a2 2 0 0 1 .6 1.4V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z",
      },
    ],
    ["path", { d: "M17 21v-7a1 1 0 0 0-1-1H8a1 1 0 0 0-1 1v7" }],
    ["path", { d: "M7 3v4a1 1 0 0 0 1 1h7" }],
  ],
  globe: [
    ["circle", { cx: "12", cy: "12", r: "10" }],
    ["path", { d: "M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20" }],
    ["path", { d: "M2 12h20" }],
  ],
  copy: [
    ["rect", { width: "14", height: "14", x: "8", y: "8", rx: "2", ry: "2" }],
    ["path", { d: "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" }],
  ],
  pencil: [
    [
      "path",
      {
        d: "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z",
      },
    ],
    ["path", { d: "m15 5 4 4" }],
  ],
  link2: [
    ["path", { d: "M9 17H7A5 5 0 0 1 7 7h2" }],
    ["path", { d: "M15 7h2a5 5 0 1 1 0 10h-2" }],
    ["line", { x1: "8", x2: "16", y1: "12", y2: "12" }],
  ],
  link2Off: [
    ["path", { d: "M9 17H7A5 5 0 0 1 7 7" }],
    ["path", { d: "M15 7h2a5 5 0 0 1 4 8" }],
    ["line", { x1: "8", x2: "12", y1: "12", y2: "12" }],
    ["line", { x1: "2", x2: "22", y1: "2", y2: "22" }],
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
