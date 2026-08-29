/** Editor-capability lifecycle calls, separate from public MCP tools. */
import type { DocumentView } from "./mcp";
import type {
  CreateReviewerConnection,
  ReviewerConnection,
  UpdateReviewerConnection,
} from "./reviewer-bridge";

export class EditorApi {
  private readonly baseUrl: string;

  constructor(
    mcpUrl: string,
    private readonly token: string,
    private readonly fetcher: typeof fetch = globalThis.fetch.bind(globalThis),
  ) {
    const url = new URL(mcpUrl);
    url.pathname = url.pathname.replace(/\/mcp\/?$/, "");
    url.search = "";
    url.hash = "";
    this.baseUrl = url.toString().replace(/\/$/, "");
  }

  async createDocument(title: string, initialMarkdown?: string): Promise<DocumentView> {
    return this.post("/editor/documents", {
      title,
      ...(initialMarkdown === undefined ? {} : { initial_markdown: initialMarkdown }),
    });
  }

  async setDocumentDeleted(docId: string, deleted: boolean): Promise<unknown> {
    return this.post(`/editor/documents/${encodeURIComponent(docId)}/deleted`, { deleted });
  }

  async listReviewerConnections(): Promise<ReviewerConnection[]> {
    const value = await this.request<{ connections: ReviewerConnection[] }>(
      "GET",
      "/editor/reviewer-connections",
    );
    if (!Array.isArray(value?.connections)) {
      throw new Error("editor returned an invalid reviewer list");
    }
    return value.connections;
  }

  async createReviewerConnection(
    input: CreateReviewerConnection,
  ): Promise<ReviewerConnection> {
    return this.connectionFrom(
      await this.request("POST", "/editor/reviewer-connections", input),
    );
  }

  async updateReviewerConnection(
    id: string,
    input: UpdateReviewerConnection,
  ): Promise<ReviewerConnection> {
    return this.connectionFrom(
      await this.request(
        "PATCH",
        `/editor/reviewer-connections/${encodeURIComponent(id)}`,
        input,
      ),
    );
  }

  async resetReviewerConnection(
    id: string,
    expectedRevision: number,
  ): Promise<ReviewerConnection> {
    return this.connectionFrom(
      await this.request(
        "POST",
        `/editor/reviewer-connections/${encodeURIComponent(id)}/reset`,
        { expected_revision: expectedRevision },
      ),
    );
  }

  async revokeReviewerConnection(
    id: string,
    expectedRevision: number,
  ): Promise<ReviewerConnection> {
    return this.connectionFrom(
      await this.request(
        "DELETE",
        `/editor/reviewer-connections/${encodeURIComponent(id)}`,
        { expected_revision: expectedRevision },
      ),
    );
  }

  private async post(path: string, body: unknown): Promise<any> {
    return this.request("POST", path, body);
  }

  private async request<T = any>(
    method: "GET" | "POST" | "PATCH" | "DELETE",
    path: string,
    body?: unknown,
  ): Promise<T> {
    const response = await this.fetcher(`${this.baseUrl}${path}`, {
      method,
      headers: {
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
        Authorization: `Bearer ${this.token}`,
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const value = (await response.json().catch(() => null)) as { error?: string } | null;
    if (!response.ok) throw new Error(value?.error || `editor request failed (${response.status})`);
    return value as T;
  }

  private connectionFrom(value: unknown): ReviewerConnection {
    const connection = (value as { connection?: ReviewerConnection } | null)?.connection;
    if (!connection || typeof connection.id !== "string") {
      throw new Error("editor returned an invalid reviewer connection");
    }
    return connection;
  }
}
