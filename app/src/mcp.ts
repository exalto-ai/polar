/**
 * A minimal MCP client for the window's own needs — listing and searching
 * documents for the ⌘K switcher.
 *
 * The switcher searches through the daemon's `search` tool, the same one agents
 * use, so there is one search implementation rather than two that disagree.
 */
/** The daemon restarted and no longer knows our session. */
class StaleSession extends Error {}

export class Mcp {
  private session: string | null = null;
  private id = 0;

  constructor(
    private readonly url: string,
    private readonly token: string,
  ) {}

  /**
   * One call, re-establishing the session if the daemon has forgotten it.
   *
   * A restarted daemon does not know our session id and answers 404 forever
   * after. Without this the window keeps a dead session for its whole lifetime
   * and every later call fails silently — the switcher simply stops listing
   * anything.
   */
  private async rpc(method: string, params: unknown): Promise<any> {
    try {
      return await this.send(method, params);
    } catch (error) {
      if (!(error instanceof StaleSession)) throw error;
      this.session = null;
      await this.handshake();
      return this.send(method, params);
    }
  }

  private async send(method: string, params: unknown): Promise<any> {
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
    // 404 means the daemon has no such session — it restarted under us.
    if (response.status === 404 && this.session) throw new StaleSession();
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
    await this.handshake();
  }

  private async handshake() {
    await this.send("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "polar", version: "0.1.0" },
    });
    await this.send("notifications/initialized", {});
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

  async documentActors(docId: string) {
    return (await this.call("document_actors", { doc_id: docId })).actors as Actor[];
  }

  async setDocumentDeleted(docId: string, deleted: boolean) {
    return await this.call("set_document_deleted", {
      doc_id: docId,
      deleted,
      agent: "window",
      model: null,
      session: null,
    });
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

export type Actor = {
  actor_id: string;
  kind: "human" | "agent";
  display_name: string;
  model: string | null;
  color: string;
  last_seen: number;
  edits: number;
};

export type DocumentSummary = { doc_id: string; title: string; updated_at: number };
export type SearchHit = { doc_id: string; title: string; snippet: string };
