/**
 * A minimal MCP client for the window's own needs — listing and searching
 * documents for the ⌘K switcher.
 *
 * The switcher searches through the daemon's `search` tool, the same one agents
 * use, so there is one search implementation rather than two that disagree.
 */
export class Mcp {
  private session: string | null = null;
  private id = 0;

  constructor(
    private readonly url: string,
    private readonly token: string,
  ) {}

  private async rpc(method: string, params: unknown): Promise<any> {
    const notification = method.startsWith("notifications/");
    const body: Record<string, unknown> = { jsonrpc: "2.0", method, params };
    if (!notification) body.id = ++this.id;

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      Authorization: `Bearer ${this.token}`,
    };
    if (this.session) headers["Mcp-Session-Id"] = this.session;

    const response = await fetch(this.url, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });
    const sessionId = response.headers.get("mcp-session-id");
    if (sessionId && !this.session) this.session = sessionId;

    const raw = await response.text();
    for (const line of raw.split("\n")) {
      if (!line.startsWith("data: ") || !line.slice(6).trim()) continue;
      const message = JSON.parse(line.slice(6));
      if (message.error) throw new Error(message.error.message ?? "mcp error");
      if (message.result) return message.result;
    }
    return null;
  }

  async connect() {
    await this.rpc("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "polar", version: "0.1.0" },
    });
    await this.rpc("notifications/initialized", {});
  }

  private async call(name: string, args: unknown) {
    const result = await this.rpc("tools/call", { name, arguments: args });
    return JSON.parse(result.content[0].text);
  }

  async listDocuments(limit = 50) {
    return (await this.call("list_documents", { limit })).documents as DocumentSummary[];
  }

  async search(query: string, limit = 20) {
    return (await this.call("search", { query, limit })).hits as SearchHit[];
  }

  async createDocument(title: string) {
    return await this.call("create_document", {
      title,
      agent: "window",
      model: null,
      session: null,
    });
  }
}

export type DocumentSummary = { doc_id: string; title: string; updated_at: number };
export type SearchHit = { doc_id: string; title: string; snippet: string };
