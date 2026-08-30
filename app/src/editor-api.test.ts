import { afterEach, describe, expect, it, vi } from "vitest";
import { EditorDocuments } from "./editor-api";

afterEach(() => vi.unstubAllGlobals());

describe("editor document lifecycle", () => {
  it("uses the editor route instead of self-asserting a human over MCP", async () => {
    const fetch = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ doc_id: "doc-1" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetch);
    const documents = new EditorDocuments("http://127.0.0.1:1234/mcp", "secret");

    await documents.createDocument("Notes", "Imported");

    expect(fetch).toHaveBeenCalledWith(
      new URL("http://127.0.0.1:1234/editor/documents"),
      expect.objectContaining({
        method: "POST",
        headers: expect.objectContaining({ Authorization: "Bearer secret" }),
        body: JSON.stringify({ title: "Notes", markdown: "Imported" }),
      }),
    );
  });

  it("reports a daemon rejection", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("invalid markdown", { status: 400 })),
    );
    const documents = new EditorDocuments("http://127.0.0.1:1234/mcp", "secret");
    await expect(documents.createDocument("Notes", "bad")).rejects.toThrow(
      "invalid markdown",
    );
  });
});
