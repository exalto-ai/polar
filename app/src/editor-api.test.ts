import { describe, expect, it, vi } from "vitest";
import { EditorApi } from "./editor-api";

function response(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("editor lifecycle API", () => {
  it("uses the editor capability and never sends caller-controlled identity", async () => {
    const fetcher = vi.fn().mockResolvedValue(response({ doc_id: "doc-1" }));
    const api = new EditorApi("http://127.0.0.1:4500/mcp", "editor-secret", fetcher);

    await api.createDocument("Notes", "# Notes");

    expect(fetcher).toHaveBeenCalledWith("http://127.0.0.1:4500/editor/documents", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer editor-secret",
      },
      body: JSON.stringify({ title: "Notes", initial_markdown: "# Notes" }),
    });
    expect(fetcher.mock.calls[0][1].body).not.toContain("agent");
    expect(fetcher.mock.calls[0][1].body).not.toContain("model");
  });

  it("encodes document IDs and surfaces daemon errors", async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce(response({ ok: true }))
      .mockResolvedValueOnce(response({ error: "not allowed" }, 401));
    const api = new EditorApi("http://localhost:9000/mcp", "editor-secret", fetcher);

    await api.setDocumentDeleted("doc/one", true);
    expect(fetcher.mock.calls[0][0]).toBe(
      "http://localhost:9000/editor/documents/doc%2Fone/deleted",
    );
    await expect(api.createDocument("")).rejects.toThrow("not allowed");
  });
});
