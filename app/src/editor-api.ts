/** Editor-capability lifecycle calls, separate from public MCP tools. */
import type { DocumentView } from "./mcp";

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

  private async post(path: string, body: unknown): Promise<any> {
    const response = await this.fetcher(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${this.token}`,
      },
      body: JSON.stringify(body),
    });
    const value = (await response.json().catch(() => null)) as { error?: string } | null;
    if (!response.ok) throw new Error(value?.error || `editor request failed (${response.status})`);
    return value;
  }
}
