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

  it("lists and decides suggestions through editor-only routes", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(Response.json({ content_revision: "rev", suggestions: [] }))
      .mockResolvedValueOnce(Response.json({ suggestion: { state: "accepted" } }))
      .mockResolvedValueOnce(Response.json({ suggestion: { state: "rejected" } }));
    vi.stubGlobal("fetch", fetch);
    const documents = new EditorDocuments("http://127.0.0.1:1234/mcp", "secret");

    await documents.listSuggestions("doc/one");
    await documents.acceptSuggestion("doc/one", "review/one");
    await documents.rejectSuggestion("doc/one", "review/one");

    expect(fetch.mock.calls.map(([url]) => url.toString())).toEqual([
      "http://127.0.0.1:1234/editor/documents/doc%2Fone/suggestions",
      "http://127.0.0.1:1234/editor/documents/doc%2Fone/suggestions/review%2Fone/accept",
      "http://127.0.0.1:1234/editor/documents/doc%2Fone/suggestions/review%2Fone/reject",
    ]);
  });

  it("submits provider text only as a pending chat suggestion", async () => {
    const fetch = vi.fn().mockResolvedValue(
      Response.json({ suggestion: { suggestion_id: "pro-chat:one" } }),
    );
    vi.stubGlobal("fetch", fetch);
    const documents = new EditorDocuments("http://127.0.0.1:1234/mcp", "secret");

    await documents.proposeChatSuggestion({
      documentId: "doc/one",
      requestId: "request-one",
      provider: "openai",
      requestedModel: "gpt-test",
      reportedModel: null,
      assistantText: "Suggested ending",
      wordingRevision: "wording-one",
      after: { kind: "end" },
    });

    expect(fetch).toHaveBeenCalledWith(
      new URL("http://127.0.0.1:1234/editor/documents/doc%2Fone/suggestions/pro-chat"),
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          request_id: "request-one",
          provider: "openai",
          requested_model: "gpt-test",
          reported_model: null,
          assistant_text: "Suggested ending",
          wording_revision: "wording-one",
          after: { kind: "end" },
        }),
      }),
    );
  });

  it("lists and decides direct-edit access through editor-only routes", async () => {
    const grant = {
      grant_id: "grant/one",
      connection_id: "connection-one",
      document_id: "doc/one",
      document_title: "Draft",
      display_label: "Reviewer",
      client: "codex",
      reported_model: null,
      granted_at: 20,
    };
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(Response.json({ requests: [], grants: [] }))
      .mockResolvedValueOnce(Response.json({ grant }))
      .mockResolvedValueOnce(Response.json({
        denial: { request_id: "request/one", retry_at: 30 },
      }))
      .mockResolvedValueOnce(Response.json({ grant }));
    vi.stubGlobal("fetch", fetch);
    const documents = new EditorDocuments("http://127.0.0.1:1234/mcp", "secret");

    await documents.listDirectEditAccess();
    await documents.approveDirectEdit("doc/one", "request/one");
    await documents.denyDirectEdit("doc/one", "request/one");
    await documents.revokeDirectEdit("doc/one", "grant/one");

    expect(fetch.mock.calls.map(([url]) => url.toString())).toEqual([
      "http://127.0.0.1:1234/editor/direct-edit-access",
      "http://127.0.0.1:1234/editor/documents/doc%2Fone/direct-edit-requests/request%2Fone/approve",
      "http://127.0.0.1:1234/editor/documents/doc%2Fone/direct-edit-requests/request%2Fone/deny",
      "http://127.0.0.1:1234/editor/documents/doc%2Fone/direct-edit-grants/grant%2Fone",
    ]);
    expect(fetch.mock.calls.map(([, init]) => init?.method)).toEqual([
      "GET",
      "POST",
      "POST",
      "DELETE",
    ]);
  });
});
