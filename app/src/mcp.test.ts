import { describe, expect, it, vi } from "vitest";
import { Mcp } from "./mcp";

function rpcResponse(value: unknown, headers: Record<string, string> = {}): Response {
  const message = {
    jsonrpc: "2.0",
    id: 1,
    result: {
      content: [{ type: "text", text: JSON.stringify(value) }],
    },
  };
  return new Response(`data: ${JSON.stringify(message)}\n\n`, {
    status: 200,
    headers: { "Content-Type": "text/event-stream", ...headers },
  });
}

describe("MCP session recovery", () => {
  it(
    "single-flights replacement after an established session expires",
    async () => {
      let initializeCount = 0;
      const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
        const body = JSON.parse(String(init?.body));
        const headers = init?.headers as Record<string, string>;
        if (body.method === "initialize") {
          initializeCount += 1;
          return rpcResponse(
            { protocolVersion: "2025-06-18" },
            { "mcp-session-id": initializeCount === 1 ? "old-session" : "new-session" },
          );
        }
        if (body.method === "notifications/initialized") {
          return new Response("", { status: 202 });
        }
        if (headers["Mcp-Session-Id"] === "old-session") {
          return new Response("", { status: 404 });
        }
        return rpcResponse({ documents: [] });
      });
      const client = new Mcp(
        "http://127.0.0.1:4317/mcp",
        "secret-token",
        fetcher as unknown as typeof fetch,
      );
      await client.connect();

      await expect(Promise.all([
        client.listDocuments(),
        client.listDocuments(),
      ])).resolves.toEqual([[], []]);

      expect(initializeCount).toBe(2);
      const toolSessions = fetcher.mock.calls
        .filter(([, callInit]) => JSON.parse(String(callInit?.body)).method === "tools/call")
        .map(([, callInit]) =>
          (callInit?.headers as Record<string, string>)["Mcp-Session-Id"]
        );
      expect(toolSessions).toEqual([
        "old-session",
        "old-session",
        "new-session",
        "new-session",
      ]);
    },
  );

  it("does not retry an authorization failure without an established session", async () => {
    const fetcher = vi.fn(async () => new Response("", { status: 401 }));
    const client = new Mcp(
      "http://127.0.0.1:4317/mcp",
      "invalid-token",
      fetcher as unknown as typeof fetch,
    );

    await expect(client.connect()).rejects.toThrow("MCP request failed (401)");
    expect(fetcher).toHaveBeenCalledOnce();
  });

  it("recovers when ensureSession expires before its initialized notification", async () => {
    const sessions = ["expired-handshake", "stable-session"];
    let initializeCount = 0;
    const initializedSessions: Array<string | undefined> = [];
    const toolSessions: Array<string | undefined> = [];
    const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body));
      const headers = init?.headers as Record<string, string>;
      if (body.method === "initialize") {
        const session = sessions[initializeCount];
        initializeCount += 1;
        return rpcResponse(
          { protocolVersion: "2025-06-18" },
          { "mcp-session-id": session },
        );
      }
      if (body.method === "notifications/initialized") {
        const session = headers["Mcp-Session-Id"];
        initializedSessions.push(session);
        return new Response("", { status: session === "expired-handshake" ? 404 : 202 });
      }
      const session = headers["Mcp-Session-Id"];
      toolSessions.push(session);
      return rpcResponse({ documents: [] });
    });
    const client = new Mcp(
      "http://127.0.0.1:4317/mcp",
      "secret-token",
      fetcher as unknown as typeof fetch,
    );
    await expect(client.listDocuments()).resolves.toEqual([]);

    expect(initializeCount).toBe(2);
    expect(initializedSessions).toEqual([
      "expired-handshake",
      "stable-session",
    ]);
    expect(toolSessions).toEqual(["stable-session"]);
  });

  it("recovers when the first replacement expires on the retried RPC", async () => {
    const sessions = ["old-session", "first-replacement", "stable-session"];
    let initializeCount = 0;
    const toolSessions: Array<string | undefined> = [];
    const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body));
      const headers = init?.headers as Record<string, string>;
      if (body.method === "initialize") {
        const session = sessions[initializeCount];
        initializeCount += 1;
        return rpcResponse(
          { protocolVersion: "2025-06-18" },
          { "mcp-session-id": session },
        );
      }
      if (body.method === "notifications/initialized") {
        return new Response("", { status: 202 });
      }
      const session = headers["Mcp-Session-Id"];
      toolSessions.push(session);
      if (session !== "stable-session") return new Response("", { status: 404 });
      return rpcResponse({ documents: [] });
    });
    const client = new Mcp(
      "http://127.0.0.1:4317/mcp",
      "secret-token",
      fetcher as unknown as typeof fetch,
    );
    await client.connect();

    await expect(client.listDocuments()).resolves.toEqual([]);

    expect(initializeCount).toBe(3);
    expect(toolSessions).toEqual([
      "old-session",
      "first-replacement",
      "stable-session",
    ]);
  });

  it("stops after two consecutive replacement sessions also expire", async () => {
    let initializeCount = 0;
    let toolCount = 0;
    const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body));
      if (body.method === "initialize") {
        initializeCount += 1;
        return rpcResponse(
          { protocolVersion: "2025-06-18" },
          { "mcp-session-id": `session-${initializeCount}` },
        );
      }
      if (body.method === "notifications/initialized") {
        return new Response("", { status: 202 });
      }
      toolCount += 1;
      return new Response("", { status: 404 });
    });
    const client = new Mcp(
      "http://127.0.0.1:4317/mcp",
      "secret-token",
      fetcher as unknown as typeof fetch,
    );
    await client.connect();

    await expect(client.listDocuments()).rejects.toThrow("the MCP session is stale");

    expect(initializeCount).toBe(3);
    expect(toolCount).toBe(3);
  });

  it("keeps a captured session while a concurrent replacement is initializing", async () => {
    let initializeCount = 0;
    let releasePausedOld!: () => void;
    const pausedOld = new Promise<void>((resolve) => {
      releasePausedOld = resolve;
    });
    let pausedOldCaptured!: () => void;
    const oldSessionWasCaptured = new Promise<void>((resolve) => {
      pausedOldCaptured = resolve;
    });
    let releaseReplacementInitialize!: () => void;
    const replacementInitialize = new Promise<void>((resolve) => {
      releaseReplacementInitialize = resolve;
    });
    let replacementInitializeStarted!: () => void;
    const replacementSessionIsPending = new Promise<void>((resolve) => {
      replacementInitializeStarted = resolve;
    });
    let oldToolCount = 0;
    const fetcher = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const body = JSON.parse(String(init?.body));
      const headers = init?.headers as Record<string, string>;
      if (body.method === "initialize") {
        initializeCount += 1;
        if (initializeCount === 2) {
          replacementInitializeStarted();
          await replacementInitialize;
        }
        return rpcResponse(
          { protocolVersion: "2025-06-18" },
          { "mcp-session-id": initializeCount === 1 ? "old-session" : "new-session" },
        );
      }
      if (body.method === "notifications/initialized") {
        return new Response("", { status: 202 });
      }
      if (headers["Mcp-Session-Id"] === "old-session") {
        oldToolCount += 1;
        if (oldToolCount === 1) {
          pausedOldCaptured();
          await pausedOld;
        }
        return new Response("", { status: 404 });
      }
      return rpcResponse({ documents: [] });
    });
    const client = new Mcp(
      "http://127.0.0.1:4317/mcp",
      "secret-token",
      fetcher as unknown as typeof fetch,
    );
    await client.connect();

    const first = client.listDocuments();
    await oldSessionWasCaptured;
    const second = client.listDocuments();
    await replacementSessionIsPending;
    releasePausedOld();
    releaseReplacementInitialize();
    await expect(Promise.all([first, second])).resolves.toEqual([[], []]);

    const toolSessions = fetcher.mock.calls
      .filter(([, callInit]) => JSON.parse(String(callInit?.body)).method === "tools/call")
      .map(([, callInit]) =>
        (callInit?.headers as Record<string, string>)["Mcp-Session-Id"]
      );
    expect(toolSessions).toEqual([
      "old-session",
      "old-session",
      "new-session",
      "new-session",
    ]);
    expect(toolSessions).not.toContain(undefined);
    expect(initializeCount).toBe(2);
  });
});
