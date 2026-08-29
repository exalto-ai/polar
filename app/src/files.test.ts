import { describe, expect, it, vi } from "vitest";
import {
  exportMarkdownDocument,
  importMarkdownDocument,
  suggestedMarkdownName,
  titleFromFileName,
  type FileBridge,
} from "./files";

describe("Markdown file names", () => {
  it("derives a fallback title from either Markdown extension", () => {
    expect(titleFromFileName("/tmp/Project notes.md")).toBe("Project notes");
    expect(titleFromFileName("C:\\notes\\Plan.MARKDOWN")).toBe("Plan");
  });

  it("makes a portable suggested export name", () => {
    expect(suggestedMarkdownName('Launch: "phase/one"?')).toBe("Launch- -phase-one--.md");
    expect(suggestedMarkdownName("   ")).toBe("Untitled.md");
  });
});

describe("Markdown file actions", () => {
  it("cancels open without creating or switching documents", async () => {
    const bridge = {
      importMarkdown: vi.fn().mockResolvedValue(null),
      exportMarkdown: vi.fn(),
    } satisfies FileBridge;
    const createDocument = vi.fn();
    const openDocument = vi.fn();

    await expect(
      importMarkdownDocument(bridge, { createDocument }, openDocument),
    ).resolves.toBeNull();
    expect(createDocument).not.toHaveBeenCalled();
    expect(openDocument).not.toHaveBeenCalled();
  });

  it("imports the complete Markdown as one new document before opening it", async () => {
    const file = { file_name: "Research.md", markdown: "# Research\n\nFindings." };
    const bridge = {
      importMarkdown: vi.fn().mockResolvedValue(file),
      exportMarkdown: vi.fn(),
    } satisfies FileBridge;
    const createDocument = vi.fn().mockResolvedValue({ doc_id: "doc-imported" });
    const openDocument = vi.fn().mockResolvedValue(undefined);

    await expect(
      importMarkdownDocument(bridge, { createDocument }, openDocument),
    ).resolves.toEqual(file);
    expect(createDocument).toHaveBeenCalledWith("Research", file.markdown);
    expect(openDocument).toHaveBeenCalledWith("doc-imported");
    expect(createDocument.mock.invocationCallOrder[0]).toBeLessThan(
      openDocument.mock.invocationCallOrder[0],
    );
  });

  it("exports the live editor tree with a suggested Markdown name", async () => {
    const exported = { file_name: "Plan.md" };
    const bridge = {
      importMarkdown: vi.fn(),
      exportMarkdown: vi.fn().mockResolvedValue(exported),
    } satisfies FileBridge;
    const document = { type: "doc", content: [{ type: "paragraph" }] };

    await expect(exportMarkdownDocument(bridge, document, "Plan")).resolves.toEqual(
      exported,
    );
    expect(bridge.exportMarkdown).toHaveBeenCalledWith(document, "Plan.md");
  });

  it("asks the native bridge for a new destination on every export", async () => {
    const bridge = {
      importMarkdown: vi.fn(),
      exportMarkdown: vi
        .fn()
        .mockResolvedValueOnce({ file_name: "First.md" })
        .mockResolvedValueOnce({ file_name: "Second.md" }),
    } satisfies FileBridge;
    const document = { type: "doc", content: [{ type: "paragraph" }] };

    await expect(exportMarkdownDocument(bridge, document, "Plan")).resolves.toEqual({
      file_name: "First.md",
    });
    await expect(exportMarkdownDocument(bridge, document, "Plan")).resolves.toEqual({
      file_name: "Second.md",
    });
    expect(bridge.exportMarkdown).toHaveBeenCalledTimes(2);
  });
});
