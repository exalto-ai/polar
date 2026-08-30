import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => {
  const invoke = vi.fn();
  const channels: Array<{ onmessage: (value: unknown) => void }> = [];
  class Channel {
    onmessage = (_value: unknown) => {};
    constructor() {
      channels.push(this);
    }
  }
  return { invoke, channels, Channel };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
  Channel: mocks.Channel,
}));

import {
  PRO_CHAT_DISCLOSURE_VERSION,
  tauriProChatBridge,
  type ProChatEvent,
  type ProChatStartRequest,
} from "./pro-chat-bridge";

describe("Pro chat bridge", () => {
  beforeEach(() => {
    mocks.invoke.mockReset().mockResolvedValue({});
    mocks.channels.splice(0);
  });

  it("uses fixed native commands and keeps the visible request nested", async () => {
    const bridge = tauriProChatBridge();
    const request: ProChatStartRequest = {
      document_id: "doc-1",
      document_title: "Draft",
      document: {
        type: "doc",
        content: [
          {
            type: "paragraph",
            content: [{ type: "text", text: "Current text" }],
          },
        ],
      },
      provider: "openai",
      expected_revision: 4,
      model: "gpt-current",
      thinking: "medium",
      message: "Visible message",
      retry_turn_id: null,
      disclosure_version: PRO_CHAT_DISCLOSURE_VERSION,
    };
    const onEvent = vi.fn();

    await bridge.capabilities();
    await bridge.history("doc-1", "anthropic");
    await bridge.start(request, onEvent);
    await bridge.stop("operation-1");
    await bridge.suggestResponse({
      documentId: "doc-1",
      provider: "openai",
      turnId: "turn-1",
      requestId: "request-1",
      after: { kind: "end" },
    });
    await bridge.clear("doc-1", "openai", 7);

    expect(mocks.invoke.mock.calls[0]).toEqual(["pro_chat_capabilities"]);
    expect(mocks.invoke.mock.calls[1]).toEqual([
      "pro_chat_history",
      { documentId: "doc-1", provider: "anthropic" },
    ]);
    expect(mocks.invoke.mock.calls[2][0]).toBe("start_pro_chat");
    expect(mocks.invoke.mock.calls[2][1]).toEqual({
      request,
      onEvent: mocks.channels[0],
    });
    expect(mocks.invoke.mock.calls[3]).toEqual([
      "stop_pro_chat",
      { operationId: "operation-1" },
    ]);
    expect(mocks.invoke.mock.calls[4]).toEqual([
      "suggest_chat_response",
      {
        request: {
          document_id: "doc-1",
          provider: "openai",
          turn_id: "turn-1",
          request_id: "request-1",
          after: { kind: "end" },
        },
      },
    ]);
    expect(mocks.invoke.mock.calls[5]).toEqual([
      "clear_pro_chat",
      { documentId: "doc-1", provider: "openai", expectedRevision: 7 },
    ]);
    expect(JSON.stringify(mocks.invoke.mock.calls)).not.toMatch(
      /api.?key|authorization|bearer|password|secret/i,
    );
  });

  it("scopes streamed events to the channel created for that invocation", async () => {
    const bridge = tauriProChatBridge();
    const onEvent = vi.fn();
    const request: ProChatStartRequest = {
      document_id: "doc-1",
      document_title: "Draft",
      document: { type: "doc", content: [{ type: "paragraph" }] },
      provider: "openai",
      expected_revision: 0,
      model: "gpt-current",
      thinking: "default",
      message: "Hello",
      retry_turn_id: null,
      disclosure_version: PRO_CHAT_DISCLOSURE_VERSION,
    };

    await bridge.start(request, onEvent);
    const event: ProChatEvent = {
      type: "delta",
      operation_id: "operation-1",
      turn_id: "turn-1",
      text: "Hi",
    };
    mocks.channels[0].onmessage(event);

    expect(onEvent).toHaveBeenCalledWith(event);
  });
});
