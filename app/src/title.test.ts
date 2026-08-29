import { generateHTML, generateJSON } from "@tiptap/core";
import { describe, expect, it } from "vitest";
import { extensions } from "./schema";

describe("title heading variant", () => {
  it("round-trips the title marker through TipTap HTML", () => {
    const json = generateJSON(
      '<h1 data-thought-variant="title">A document title</h1>',
      extensions,
    );

    expect(json.content?.[0]).toMatchObject({
      type: "heading",
      attrs: { level: 1, variant: "title" },
    });
    expect(generateHTML(json, extensions)).toContain(
      '<h1 data-thought-variant="title">A document title</h1>',
    );
  });

  it("keeps an ordinary H1 distinct from a title", () => {
    const json = generateJSON("<h1>An ordinary heading</h1>", extensions);

    expect(json.content?.[0]).toMatchObject({
      type: "heading",
      attrs: { level: 1, variant: null },
    });
    expect(generateHTML(json, extensions)).toBe("<h1>An ordinary heading</h1>");
  });
});
