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
  it.each([404, 401])(
    "single-flights replacement after an established session fails with %i",
    async (staleStatus) => {
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
          return new Response("", { status: staleStatus });
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
