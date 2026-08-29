/**
 * A minimal MCP client for the window's own needs — listing and searching
 * documents for the ⌘K switcher.
 *
 * The switcher searches through the daemon's `search` tool, the same one agents
 * use, so there is one search implementation rather than two that disagree.
 */
/** The daemon forgot, expired, or evicted our established session. */
class StaleSession extends Error {
  constructor(readonly sentSession: string) {
    super("the MCP session is stale");
  }
}

export class Mcp {
  private session: string | null = null;
  private handshakePromise: Promise<string> | null = null;
  private id = 0;

  constructor(
    private readonly url: string,
    private readonly token: string,
    private readonly fetcher: typeof fetch = globalThis.fetch.bind(globalThis),
  ) {}

  /**
   * One call, re-establishing the session if the daemon has forgotten it.
   *
   * An idle or evicted session can disappear while the window remains open.
   * The MCP transport can answer 404, while the editor's authorization layer
   * answers 401 before the request reaches that transport. Without recovery
   * the window keeps a dead session and every later call fails.
  */
  private async rpc(method: string, params: unknown): Promise<any> {
    const sentSession = await this.ensureSession();
    try {
      return (await this.send(method, params, sentSession)).result;
    } catch (error) {
      if (!(error instanceof StaleSession)) throw error;
      const replacementSession = await this.recoverSession(error.sentSession);
      return (await this.send(method, params, replacementSession)).result;
    }
  }

  private async send(
    method: string,
    params: unknown,
    sentSession: string | null,
  ): Promise<{ result: any; sessionId: string | null }> {
    const notification = method.startsWith("notifications/");
    const body: Record<string, unknown> = { jsonrpc: "2.0", method, params };
    if (!notification) body.id = ++this.id;

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      Authorization: `Bearer ${this.token}`,
    };
    if (sentSession) headers["Mcp-Session-Id"] = sentSession;

    const response = await this.fetcher(this.url, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });
    // A 404 comes from the MCP transport and a 401 comes from the editor's
    // session binding. Either status means this established session may have
    // disappeared after idle expiry, eviction, or a binding reset. Retry only
    // when a session was sent, so an invalid bearer on initialize stays a hard
    // authorization failure.
    if ((response.status === 404 || response.status === 401) && sentSession) {
      throw new StaleSession(sentSession);
    }
    if (!response.ok) throw new Error(`MCP request failed (${response.status})`);
    const sessionId = response.headers.get("mcp-session-id");

    const raw = await response.text();
    for (const line of raw.split("\n")) {
      if (!line.startsWith("data: ") || !line.slice(6).trim()) continue;
      const message = JSON.parse(line.slice(6));
      if (message.error) throw new Error(message.error.message ?? "mcp error");
      if ("result" in message) return { result: message.result, sessionId };
    }
    return { result: null, sessionId };
  }

  async connect() {
    await this.ensureSession();
  }

  private async ensureSession(): Promise<string> {
    if (this.handshakePromise !== null) return this.handshakePromise;
    if (this.session !== null) return this.session;
    const pending = this.handshake();
    this.handshakePromise = pending;
    try {
      const establishedSession = await pending;
      if (this.handshakePromise === pending) this.session = establishedSession;
      return establishedSession;
    } finally {
      if (this.handshakePromise === pending) this.handshakePromise = null;
    }
  }

  private async recoverSession(sentSession: string): Promise<string> {
    // A second stale response arriving while the replacement initializes must
    // join the whole handshake instead of retrying on a half-ready session.
    if (this.handshakePromise !== null) {
      return this.handshakePromise;
    }
    // Another request may already have replaced the exact session that failed.
    // Never clear that newer session or start a competing handshake.
    if (this.session !== null && this.session !== sentSession) return this.session;
    if (this.session === sentSession) {
      this.session = null;
    }
    return this.ensureSession();
  }

  private async handshake(): Promise<string> {
    const initialized = await this.send("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "thought", version: "0.1.0" },
    }, null);
    if (initialized.sessionId === null) {
      throw new Error("MCP initialize response did not establish a session");
    }
    await this.send("notifications/initialized", {}, initialized.sessionId);
    return initialized.sessionId;
  }

  private async call(name: string, args: unknown) {
    const result = await this.rpc("tools/call", { name, arguments: args });
    return JSON.parse(result.content[0].text);
  }

  async listDocuments(limit = 50, trashed = false) {
    return (await this.call("list_documents", { limit, trashed }))
      .documents as DocumentSummary[];
  }

  async search(query: string, limit = 20) {
    return (await this.call("search", { query, limit })).hits as SearchHit[];
  }

  async documentActors(docId: string) {
    return (await this.call("document_actors", { doc_id: docId })).actors as Actor[];
  }

  async readDocument(docId: string): Promise<DocumentView> {
    return await this.call("read_document", { doc_id: docId });
  }

  async blockProvenance(docId: string) {
    return (await this.call("block_provenance", { doc_id: docId }))
      .blocks as BlockAttribution[];
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

/** Who wrote one block. `created_by` and `touched_by` differ once someone
 *  edits someone else's text, and the rails' label says so. */
export type BlockAttribution = {
  block_id: string;
  created_by: string;
  created_at: number;
  touched_by: string;
  touched_at: number;
  session_id: string | null;
  kind: "human" | "agent";
  display_name: string;
  model: string | null;
  color: string;
};

export type DocumentSummary = { doc_id: string; title: string; updated_at: number };
export type SearchHit = { doc_id: string; title: string; snippet: string };
export type DocumentView = {
  doc_id: string;
  title: string;
  markdown: string;
  version: string;
  blocks: Array<{
    block_id: string;
    kind: string;
    line_start: number;
    line_end: number;
  }>;
};
