import { Editor, generateHTML, generateJSON } from "@tiptap/core";
import { describe, expect, it } from "vitest";
import { MAX_FONT_SIZE_PX, MIN_FONT_SIZE_PX, normalizeFontSize } from "./font-size";
import { extensions } from "./schema";

describe("font size mark", () => {
  it("normalizes only safe whole-pixel values", () => {
    expect(normalizeFontSize(`${MIN_FONT_SIZE_PX}px`)).toBe(`${MIN_FONT_SIZE_PX}px`);
    expect(normalizeFontSize("18px")).toBe("18px");
    expect(normalizeFontSize(` ${MAX_FONT_SIZE_PX}px `)).toBe(`${MAX_FONT_SIZE_PX}px`);

    expect(normalizeFontSize(`${MIN_FONT_SIZE_PX - 1}px`)).toBeNull();
    expect(normalizeFontSize(`${MAX_FONT_SIZE_PX + 1}px`)).toBeNull();
    expect(normalizeFontSize("18.5px")).toBeNull();
    expect(normalizeFontSize("1rem")).toBeNull();
    expect(normalizeFontSize("18px; color: red")).toBeNull();
  });

  it("parses a styled span into the public fontSize mark", () => {
    const json = generateJSON('<p><span style="font-size: 18px">Large</span></p>', extensions);

    expect(json.content?.[0].content?.[0]).toMatchObject({
      type: "text",
      text: "Large",
      marks: [{ type: "fontSize", attrs: { size: "18px" } }],
    });
  });

  it("renders the mark as the stable HTML span syntax", () => {
    const html = generateHTML(
      {
        type: "doc",
        content: [
          {
            type: "paragraph",
            content: [
              {
                type: "text",
                text: "Large",
                marks: [{ type: "fontSize", attrs: { size: "18px" } }],
              },
            ],
          },
        ],
      },
      extensions,
    );

    expect(html).toContain('<span style="font-size: 18px;">Large</span>');
  });

  it("supports the toolbar's generic setMark and unsetMark contract", () => {
    const editor = new Editor({ extensions, content: "<p>Large</p>" });
    editor.commands.setTextSelection({ from: 1, to: 6 });

    expect(editor.chain().focus().setMark("fontSize", { size: "18px" }).run()).toBe(true);
    expect(editor.getJSON().content?.[0].content?.[0].marks).toEqual([
      { type: "fontSize", attrs: { size: "18px" } },
    ]);

    expect(editor.chain().focus().unsetMark("fontSize").run()).toBe(true);
    expect(editor.getJSON().content?.[0].content?.[0].marks).toBeUndefined();
    editor.destroy();
  });

  it("does not import unsafe CSS as a font size mark", () => {
    const json = generateJSON(
      '<p><span style="font-size: 18px; color: red">Safe</span>' +
        '<span style="font-size: calc(1px + 1rem)">Plain</span></p>',
      extensions,
    );

    expect(json.content?.[0].content?.[0]).toMatchObject({
      text: "Safe",
      marks: [{ type: "fontSize", attrs: { size: "18px" } }],
    });
    expect(json.content?.[0].content?.[1]).toMatchObject({ text: "Plain" });
    expect(json.content?.[0].content?.[1].marks).toBeUndefined();
  });
});
