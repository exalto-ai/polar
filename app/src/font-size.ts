import { Mark } from "@tiptap/core";

/**
 * Inline font sizing shared by the editor and the Markdown projection.
 *
 * Keeping the value as a canonical pixel string makes the Yjs attribute
 * deterministic and prevents arbitrary CSS from entering rendered documents.
 */
export const MIN_FONT_SIZE_PX = 8;
export const MAX_FONT_SIZE_PX = 96;

export function normalizeFontSize(value: unknown): string | null {
  if (typeof value !== "string") return null;

  const match = /^(\d{1,3})px$/.exec(value.trim());
  if (!match) return null;

  const pixels = Number(match[1]);
  if (!Number.isInteger(pixels) || pixels < MIN_FONT_SIZE_PX || pixels > MAX_FONT_SIZE_PX) {
    return null;
  }

  return `${pixels}px`;
}

export const FontSize = Mark.create({
  name: "fontSize",

  addAttributes() {
    return {
      size: {
        isRequired: true,
        parseHTML: (element: HTMLElement) => normalizeFontSize(element.style.fontSize),
        renderHTML: (attributes: Record<string, unknown>) => {
          const size = normalizeFontSize(attributes.size);
          return size ? { style: `font-size: ${size}` } : {};
        },
        validate: (value: unknown) => {
          if (normalizeFontSize(value) !== value) {
            throw new RangeError(
              `fontSize.size must be a whole pixel value from ${MIN_FONT_SIZE_PX}px to ${MAX_FONT_SIZE_PX}px`,
            );
          }
        },
      },
    };
  },

  parseHTML() {
    return [
      {
        tag: "span[style]",
        getAttrs: (element) =>
          normalizeFontSize((element as HTMLElement).style.fontSize) ? null : false,
      },
    ];
  },

  renderHTML({ HTMLAttributes }) {
    return ["span", HTMLAttributes, 0];
  },
});
