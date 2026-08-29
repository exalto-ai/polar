import { describe, expect, it, vi } from "vitest";
import { EditorApi } from "./editor-api";
import type { ReviewerConnection, ReviewerPermissions } from "./reviewer-bridge";

function response(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("editor lifecycle API", () => {
  it("uses the platform bearer and never sends caller-controlled identity", async () => {
    const fetcher = vi.fn().mockResolvedValue(response({ doc_id: "doc-1" }));
    const api = new EditorApi("http://127.0.0.1:4500/mcp", "editor-secret", fetcher);

    await api.createDocument("Notes", "# Notes");

    expect(fetcher).toHaveBeenCalledWith("http://127.0.0.1:4500/editor/documents", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer editor-secret",
      },
      body: JSON.stringify({ title: "Notes", markdown: "# Notes" }),
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
      "http://localhost:9000/editor/documents/doc%2Fone/deletion",
    );
    await expect(api.createDocument("")).rejects.toThrow("not allowed");
  });
});

const permissions: ReviewerPermissions = {
  document_scope: "current",
  can_read: true,
  can_edit: true,
  can_create: false,
  can_trash: false,
  document_ids: ["doc-1"],
};

function reviewer(overrides: Partial<ReviewerConnection> = {}): ReviewerConnection {
  return {
    id: "reviewer/one",
    client: "chatgpt",
    provider: "openai",
    display_label: "ChatGPT",
    status: "configured",
    permissions,
    revision: 3,
    created_at: 1,
    first_connected_at: null,
    last_seen_at: null,
    failure_code: null,
    revoked_at: null,
    reported_model: null,
    ...overrides,
  };
}

describe("reviewer connection API", () => {
  it("lists through the window API using the response envelope", async () => {
    const connection = reviewer();
    const fetcher = vi.fn().mockResolvedValue(response({ connections: [connection] }));
    const api = new EditorApi("http://127.0.0.1:4500/mcp", "editor-secret", fetcher);

    await expect(api.listReviewerConnections()).resolves.toEqual([connection]);
    expect(fetcher).toHaveBeenCalledWith(
      "http://127.0.0.1:4500/editor/reviewer-connections",
      {
        method: "GET",
        headers: { Authorization: "Bearer editor-secret" },
      },
    );
  });

  it("creates with nested permissions and no provider or credential", async () => {
    const connection = reviewer();
    const fetcher = vi.fn().mockImplementation(async () => response({ connection }));
    const api = new EditorApi("http://127.0.0.1:4500/mcp", "editor-secret", fetcher);

    await api.createReviewerConnection({
      client: "chatgpt",
      display_label: "ChatGPT",
      permissions,
    });

    const request = fetcher.mock.calls[0][1];
    expect(request.method).toBe("POST");
    expect(request.body).toBe(
      JSON.stringify({ client: "chatgpt", display_label: "ChatGPT", permissions }),
    );
    expect(request.body).not.toContain("provider");
    expect(request.body).not.toContain("token");
    expect(request.body).not.toContain("secret");
  });

  it("uses encoded IDs and optimistic revisions for update, reset, and revoke", async () => {
    const connection = reviewer();
    const fetcher = vi.fn().mockImplementation(async () => response({ connection }));
    const api = new EditorApi("http://127.0.0.1:4500/mcp", "editor-secret", fetcher);

    await api.updateReviewerConnection(connection.id, {
      expected_revision: 3,
      display_label: "Editor",
      permissions: { ...permissions, can_trash: true },
    });
    await api.resetReviewerConnection(connection.id, 4);
    await api.revokeReviewerConnection(connection.id, 5);

    expect(fetcher.mock.calls.map(([url]) => url)).toEqual([
      "http://127.0.0.1:4500/editor/reviewer-connections/reviewer%2Fone",
      "http://127.0.0.1:4500/editor/reviewer-connections/reviewer%2Fone/reset",
      "http://127.0.0.1:4500/editor/reviewer-connections/reviewer%2Fone",
    ]);
    expect(fetcher.mock.calls.map(([, init]) => init.method)).toEqual([
      "PATCH",
      "POST",
      "DELETE",
    ]);
    expect(fetcher.mock.calls[1][1].body).toBe(JSON.stringify({ expected_revision: 4 }));
    expect(fetcher.mock.calls[2][1].body).toBe(JSON.stringify({ expected_revision: 5 }));
  });

  it("rejects malformed successful envelopes", async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce(response({ reviewers: [] }))
      .mockResolvedValueOnce(response({ ok: true }));
    const api = new EditorApi("http://127.0.0.1:4500/mcp", "editor-secret", fetcher);

    await expect(api.listReviewerConnections()).rejects.toThrow("invalid reviewer list");
    await expect(
      api.createReviewerConnection({
        client: "codex",
        display_label: "Codex",
        permissions,
      }),
    ).rejects.toThrow("invalid reviewer connection");
  });
});
