import { Extension } from "@tiptap/core";

/**
 * A title is semantically an h1, but it is not the same presentation as an H1.
 *
 * Keeping the distinction as a heading attribute means the toolbar can switch
 * between Title and H1 without inventing a second, incompatible block node.
 * The Rust markdown projection carries this attribute with `TITLE_MARKER`.
 */
export const TitleVariant = Extension.create({
  name: "titleVariant",

  addGlobalAttributes() {
    return [
      {
        types: ["heading"],
        attributes: {
          variant: {
            default: null,
            parseHTML: (element) =>
              element.getAttribute("data-thought-variant") === "title" ? "title" : null,
            renderHTML: (attributes) =>
              attributes.variant === "title" ? { "data-thought-variant": "title" } : {},
          },
        },
      },
    ];
  },
});
