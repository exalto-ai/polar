import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import markup from "../index.html?raw";
import {
  installProChat,
  type ProChatController,
  type ProChatDocumentContext,
} from "./pro-chat";
import type {
  ProChatBridge,
  ProChatCapabilities,
  ProChatEvent,
  ProChatHistory,
  ProChatProvider,
  ProChatTurn,
} from "./pro-chat-bridge";

function fixture(): void {
  document.body.innerHTML = markup.match(/<body>([\s\S]*?)<\/body>/)?.[1] ?? "";
}

function turn(
  id: string,
  overrides: Partial<ProChatTurn> = {},
): ProChatTurn {
  return {
    id,
    user_text: "Hello",
    assistant_text: "",
    status: "pending",
    provider: "openai",
    requested_model: "gpt-current",
    reported_model: null,
    thinking: "medium",
    created_at: 1_777_000_000,
    completed_at: null,
    request_id: null,
    error_category: null,
    retryable: false,
    input_tokens: null,
    output_tokens: null,
    wording_revision: "",
    ...overrides,
  };
}

function history(
  documentId = "doc-1",
  provider: ProChatProvider = "openai",
  turns: ProChatTurn[] = [],
  revision = 0,
): ProChatHistory {
  return { document_id: documentId, provider, revision, turns };
}

function documentContext(
  id = "doc-1",
  title = "Draft",
  snapshot: () => unknown = () => ({ type: "doc", content: [] }),
): ProChatDocumentContext {
  return {
    id,
    title,
    snapshot,
    suggestionPosition: () => ({ kind: "end" }),
    waitUntilSaved: () => Promise.resolve(true),
  };
}

function capabilities() {
  return {
    providers: [
      {
        provider: "openai" as const,
        display_name: "OpenAI",
        status: "ready",
        models: [
          {
            id: "gpt-current",
            display_name: "GPT Current",
            thinking_levels: ["default", "low", "medium"] as const,
          },
          {
            id: "gpt-fast",
            display_name: "GPT Fast",
            thinking_levels: ["default", "low"] as const,
          },
        ],
      },
      {
        provider: "anthropic" as const,
        display_name: "Anthropic",
        status: "ready",
        models: [
          {
            id: "claude-current",
            display_name: "Claude Current",
            thinking_levels: ["default", "high"] as const,
          },
        ],
      },
    ].map((provider) => ({
      ...provider,
      models: provider.models.map((model) => ({
        ...model,
        thinking_levels: [...model.thinking_levels],
      })),
    })),
  };
}

function bridge(overrides: Partial<ProChatBridge> = {}): ProChatBridge {
  return {
    capabilities: vi.fn().mockResolvedValue(capabilities()),
    history: vi.fn().mockImplementation((documentId, provider) =>
      Promise.resolve(history(documentId, provider))),
    start: vi.fn().mockResolvedValue({ operation_id: "operation-1", turn_id: "turn-1" }),
    stop: vi.fn().mockResolvedValue(true),
    suggestResponse: vi.fn().mockResolvedValue({ suggestion_id: "suggestion-1" }),
    clear: vi.fn().mockImplementation((documentId, provider) =>
      Promise.resolve(history(documentId, provider, [], 2))),
    ...overrides,
  };
}

function enableComposer(): void {
  const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
  consent.checked = true;
  consent.dispatchEvent(new Event("change", { bubbles: true }));
  const message = document.querySelector<HTMLTextAreaElement>("#pro-chat-message")!;
  message.value = "Visible message";
  message.dispatchEvent(new Event("input", { bubbles: true }));
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

let controller: ProChatController | null = null;

beforeEach(fixture);
afterEach(() => {
  controller?.destroy();
  controller = null;
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("Pro chat", () => {
  it("keeps Send unavailable until the host publishes a hydrated document", async () => {
    const chatBridge = bridge();
    controller = installProChat(document, { bridge: chatBridge });
    controller.setActive(true);

    await vi.waitFor(() => expect(chatBridge.capabilities).toHaveBeenCalledTimes(1));
    expect(document.querySelector<HTMLElement>("#pro-chat-composer")!.hidden).toBe(true);
    expect(document.querySelector("#pro-chat-unavailable span")?.textContent).toContain(
      "after the document finishes opening",
    );
    expect(chatBridge.history).not.toHaveBeenCalled();
    expect(chatBridge.start).not.toHaveBeenCalled();

    controller.setDocument(documentContext());
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-1", "openai"));
    expect(document.querySelector<HTMLElement>("#pro-chat-composer")!.hidden).toBe(false);
  });

  it("requires separate visible consent and streams safe text", async () => {
    let onEvent: ((event: ProChatEvent) => void) | null = null;
    const activity = vi.fn();
    const chatBridge = bridge({
      start: vi.fn().mockImplementation(async (_request, callback) => {
        onEvent = callback;
        return { operation_id: "operation-1", turn_id: "turn-1" };
      }),
    });
    controller = installProChat(document, {
      bridge: chatBridge,
      onActivityChange: activity,
    });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-1", "openai"));

    const message = document.querySelector<HTMLTextAreaElement>("#pro-chat-message")!;
    const send = document.querySelector<HTMLButtonElement>("#pro-chat-send")!;
    message.value = "Visible message";
    message.dispatchEvent(new Event("input", { bubbles: true }));
    expect(send.disabled).toBe(true);
    expect(document.querySelector("#pro-chat-sharing")?.textContent).toContain(
      "To OpenAI: this document’s title and contents",
    );
    enableComposer();
    expect(send.disabled).toBe(false);
    expect(document.querySelector<HTMLElement>(".pro-chat-consent")!.hidden).toBe(true);
    expect(document.querySelector<HTMLElement>("#pro-chat-sharing")!.hidden).toBe(true);
    expect(document.querySelector<HTMLElement>("#pro-chat-billing-reminder")!.hidden).toBe(false);
    expect(document.querySelector("#pro-chat-billing-reminder")?.textContent).toContain(
      "Sends this document’s title and contents with each request",
    );
    send.click();

    await vi.waitFor(() => expect(chatBridge.start).toHaveBeenCalledTimes(1));
    expect(chatBridge.start).toHaveBeenCalledWith(
      {
        document_id: "doc-1",
        document_title: "Draft",
        document: { type: "doc", content: [] },
        provider: "openai",
        expected_revision: 0,
        model: "gpt-current",
        thinking: "default",
        message: "Visible message",
        retry_turn_id: null,
        disclosure_version: 2,
      },
      expect.any(Function),
    );
    expect(activity).toHaveBeenLastCalledWith(true);

    const pending = turn("turn-1", { user_text: "Visible message", thinking: "default" });
    onEvent!({
      type: "started",
      operation_id: "operation-1",
      turn: pending,
      revision: 1,
    });
    onEvent!({
      type: "delta",
      operation_id: "operation-1",
      turn_id: "turn-1",
      text: "<img src=x onerror=alert(1)>",
    });
    expect(document.querySelector(".pro-chat-message.assistant p")?.textContent).toBe(
      "<img src=x onerror=alert(1)>",
    );
    expect(document.querySelector(".pro-chat-message.assistant img")).toBeNull();

    onEvent!({
      type: "completed",
      operation_id: "operation-1",
      turn: turn("turn-1", {
        user_text: "Visible message",
        assistant_text: "Safe response",
        status: "completed",
        thinking: "default",
        reported_model: "gpt-reported",
      }),
      revision: 2,
    });
    expect(document.querySelector(".pro-chat-message-meta")?.textContent).toBe(
      "Provider reported gpt-reported",
    );
    expect(document.querySelector("#pro-chat-sharing")?.textContent).toContain(
      "2 completed earlier chat messages",
    );
    expect(activity).toHaveBeenLastCalledWith(false);
  });

  it("copies returned text and hands focus back to the editor", async () => {
    const copyText = vi.fn().mockResolvedValue(undefined);
    const onResponseCopied = vi.fn();
    const onNotice = vi.fn();
    const chatBridge = bridge({
      history: vi.fn().mockResolvedValue(history("doc-1", "openai", [
        turn("turn-1", {
          assistant_text: "First line\n\nSecond line",
          status: "completed",
        }),
      ], 1)),
    });
    controller = installProChat(document, {
      bridge: chatBridge,
      copyText,
      onResponseCopied,
      onNotice,
    });
    controller.setDocument(documentContext());
    controller.setActive(true);

    await vi.waitFor(() => expect(
      document.querySelector<HTMLButtonElement>(".pro-chat-reply-actions button"),
    ).not.toBeNull());
    const buttons = [...document.querySelectorAll<HTMLButtonElement>(
      ".pro-chat-reply-actions button",
    )];
    const byText = (label: string) => buttons.find(({ textContent }) => textContent === label)!;
    const copy = byText("Copy");
    copy.click();
    await vi.waitFor(() => expect(copyText).toHaveBeenCalledWith("First line\n\nSecond line"));
    expect(copy.textContent).toBe("Copied");
    expect(onNotice).toHaveBeenCalledWith(
      "Copied. Press Command-V to paste in the editor.",
    );
    expect(onResponseCopied).toHaveBeenCalledOnce();
    expect(buttons.map(({ textContent }) => textContent)).not.toContain("Insert at cursor");
    expect(buttons.map(({ textContent }) => textContent)).not.toContain("Replace document");
  });

  it("creates a pending suggestion from a saved completed turn", async () => {
    const suggestResponse = vi.fn().mockResolvedValue({ suggestion_id: "suggestion-1" });
    const onSuggestionCreated = vi.fn();
    const onNotice = vi.fn();
    const chatBridge = bridge({
      suggestResponse,
      history: vi.fn().mockResolvedValue(history("doc-1", "openai", [
        turn("turn-1", {
          assistant_text: "Suggested paragraph",
          status: "completed",
          wording_revision: "wording-revision",
        }),
      ], 1)),
    });
    controller = installProChat(document, {
      bridge: chatBridge,
      createRequestId: () => "request-1",
      onSuggestionCreated,
      onNotice,
    });
    controller.setDocument(documentContext());
    controller.setActive(true);

    const suggest = await vi.waitFor(() => {
      const button = [...document.querySelectorAll<HTMLButtonElement>(
        ".pro-chat-reply-actions button",
      )].find(({ textContent }) => textContent === "Suggest in document");
      expect(button).toBeTruthy();
      return button!;
    });
    suggest.click();

    await vi.waitFor(() => expect(suggestResponse).toHaveBeenCalledWith({
      documentId: "doc-1",
      provider: "openai",
      turnId: "turn-1",
      requestId: "request-1",
      after: { kind: "end" },
    }));
    expect(onSuggestionCreated).toHaveBeenCalledWith("suggestion-1");
    expect(onNotice).toHaveBeenCalledWith("Suggestion added for review.");
    expect(suggest.textContent).toBe("Suggested");
  });

  it("stops a stream and retries the same turn with its original settings", async () => {
    let liveDocument: unknown = { type: "doc", content: [{ type: "paragraph" }] };
    const eventSinks: Array<(event: ProChatEvent) => void> = [];
    const start = vi.fn().mockImplementation(async (_request, callback) => {
      eventSinks.push(callback);
      const index = eventSinks.length;
      return { operation_id: `operation-${index}`, turn_id: `turn-${index}` };
    });
    const chatBridge = bridge({ start });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext("doc-1", "Draft", () => liveDocument));
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalled());
    enableComposer();
    document.querySelector<HTMLButtonElement>("#pro-chat-send")!.click();
    await vi.waitFor(() => expect(start).toHaveBeenCalledTimes(1));

    const pending = turn("turn-1", {
      user_text: "Visible message",
      requested_model: "gpt-current",
      thinking: "medium",
    });
    eventSinks[0]({
      type: "started",
      operation_id: "operation-1",
      turn: pending,
      revision: 1,
    });
    document.querySelector<HTMLButtonElement>("#pro-chat-stop")!.click();
    await vi.waitFor(() => expect(chatBridge.stop).toHaveBeenCalledWith("operation-1"));
    eventSinks[0]({
      type: "stopped",
      operation_id: "operation-1",
      turn: {
        ...pending,
        assistant_text: "Partial response",
        status: "stopped",
        retryable: true,
      },
      revision: 2,
    });
    expect(document.querySelector(".pro-chat-message.assistant p")?.textContent).toBe(
      "Partial response",
    );
    expect(document.querySelector(".pro-chat-turn-status")?.textContent).toContain(
      "API charges may still apply",
    );
    expect(document.querySelector("#pro-chat-sharing")?.textContent).toContain(
      "To OpenAI: this document’s title and contents",
    );
    expect(document.querySelector("#pro-chat-sharing")?.textContent).not.toContain(
      "completed earlier chat messages",
    );

    const model = document.querySelector<HTMLSelectElement>("#pro-chat-model")!;
    model.value = JSON.stringify(["openai", "gpt-fast"]);
    model.dispatchEvent(new Event("change", { bubbles: true }));
    liveDocument = {
      type: "doc",
      content: [{ type: "paragraph", content: [{ type: "text", text: "Changed" }] }],
    };
    document.querySelector<HTMLButtonElement>(".pro-chat-turn-retry")!.click();
    await vi.waitFor(() => expect(start).toHaveBeenCalledTimes(2));
    expect(start.mock.calls[0][0].document).toEqual({
      type: "doc",
      content: [{ type: "paragraph" }],
    });
    expect(start.mock.calls[1][0].document).toEqual(liveDocument);
    expect(start.mock.calls[1][0]).toMatchObject({
      message: null,
      retry_turn_id: "turn-1",
      model: "gpt-current",
      thinking: "medium",
      expected_revision: 2,
    });
  });

  it("uses Enter to send without stealing Shift Enter or IME composition", async () => {
    const chatBridge = bridge();
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalled());
    enableComposer();
    const message = document.querySelector<HTMLTextAreaElement>("#pro-chat-message")!;

    message.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Enter",
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    }));
    message.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Enter",
      isComposing: true,
      bubbles: true,
      cancelable: true,
    }));
    expect(chatBridge.start).not.toHaveBeenCalled();

    message.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Enter",
      bubbles: true,
      cancelable: true,
    }));
    await vi.waitFor(() => expect(chatBridge.start).toHaveBeenCalledTimes(1));
  });

  it("reloads a stale conversation before asking the user to Send again", async () => {
    const historyRequest = vi.fn()
      .mockResolvedValueOnce(history("doc-1", "openai", [], 3))
      .mockResolvedValueOnce(history("doc-1", "openai", [
        turn("other-turn", {
          user_text: "From another window",
          assistant_text: "Updated answer",
          status: "completed",
        }),
      ], 4));
    const start = vi.fn()
      .mockRejectedValueOnce(new Error("This conversation changed. Reload it before sending."))
      .mockResolvedValueOnce({ operation_id: "operation-2", turn_id: "turn-2" });
    const chatBridge = bridge({ history: historyRequest, start });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(historyRequest).toHaveBeenCalledTimes(1));
    enableComposer();
    document.querySelector<HTMLButtonElement>("#pro-chat-send")!.click();

    await vi.waitFor(() => expect(historyRequest).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent).toContain(
      "choose Send again",
    ));
    expect(start).toHaveBeenCalledTimes(1);
    expect(document.querySelector<HTMLTextAreaElement>("#pro-chat-message")!.value).toBe(
      "Visible message",
    );
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-error-retry")!.hidden).toBe(true);

    document.querySelector<HTMLButtonElement>("#pro-chat-send")!.click();
    await vi.waitFor(() => expect(start).toHaveBeenCalledTimes(2));
    expect(start.mock.calls[1][0]).toMatchObject({
      expected_revision: 4,
      message: "Visible message",
    });
  });

  it("uses the native UTF-8 byte limit before enabling Send", async () => {
    const chatBridge = bridge();
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalled());
    const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
    consent.checked = true;
    consent.dispatchEvent(new Event("change", { bubbles: true }));
    const message = document.querySelector<HTMLTextAreaElement>("#pro-chat-message")!;
    const send = document.querySelector<HTMLButtonElement>("#pro-chat-send")!;

    message.value = "😀".repeat(4_097);
    message.dispatchEvent(new Event("input", { bubbles: true }));
    expect(message.value.length).toBeLessThan(message.maxLength);
    expect(send.disabled).toBe(true);
    expect(message.getAttribute("aria-invalid")).toBe("true");
    expect(document.querySelector("#pro-chat-message-issue")?.textContent).toContain(
      "16 KiB or less",
    );

    message.value = "😀".repeat(4_096);
    message.dispatchEvent(new Event("input", { bubbles: true }));
    expect(send.disabled).toBe(false);
    expect(message.getAttribute("aria-invalid")).toBe("false");
  });

  it("shows a terminal failure reason alongside partial response text", async () => {
    const chatBridge = bridge({
      history: vi.fn().mockResolvedValue(history("doc-1", "openai", [
        turn("turn-1", {
          assistant_text: "A partial answer",
          status: "incomplete",
          error_category: "billing",
          retryable: true,
        }),
      ], 2)),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);

    await vi.waitFor(() => expect(document.querySelector(".pro-chat-message.assistant p")?.textContent).toBe(
      "A partial answer",
    ));
    expect(document.querySelector(".pro-chat-turn-status")?.textContent).toContain(
      "billing, credit, or usage-limit problem",
    );
  });

  it("explains that a provider refusal is excluded from later context", async () => {
    const chatBridge = bridge({
      history: vi.fn().mockImplementation((documentId, provider) =>
        Promise.resolve(provider === "anthropic"
          ? history(documentId, "anthropic", [
              turn("turn-1", {
                provider: "anthropic",
                requested_model: "claude-current",
                status: "failed",
                error_category: "refusal",
                retryable: true,
              }),
            ], 2)
          : history(documentId, "openai"))),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalled());

    const model = document.querySelector<HTMLSelectElement>("#pro-chat-model")!;
    model.value = JSON.stringify(["anthropic", "claude-current"]);
    model.dispatchEvent(new Event("change", { bubbles: true }));

    await vi.waitFor(() => expect(document.querySelector(".pro-chat-message.assistant p")?.textContent).toContain(
      "provider declined this request",
    ));
    expect(document.querySelector(".pro-chat-message.assistant p")?.textContent).toContain(
      "not be included in later chat context",
    );
  });

  it("resolves initial availability only after the first successful capability check", async () => {
    const onInitialAvailabilityResolved = vi.fn();
    const capabilitiesRequest = vi.fn()
      .mockRejectedValueOnce(new Error("temporary provider lookup failure"))
      .mockResolvedValueOnce(capabilities())
      .mockResolvedValueOnce({ providers: [] });
    const chatBridge = bridge({ capabilities: capabilitiesRequest });
    controller = installProChat(document, {
      bridge: chatBridge,
      onInitialAvailabilityResolved,
    });
    controller.setDocument(documentContext());
    controller.setActive(true);

    await vi.waitFor(() => expect(capabilitiesRequest).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent).toContain(
      "temporary provider lookup failure",
    ));
    expect(onInitialAvailabilityResolved).not.toHaveBeenCalled();

    await controller.refreshCapabilities();
    expect(onInitialAvailabilityResolved).toHaveBeenCalledOnce();
    expect(onInitialAvailabilityResolved).toHaveBeenLastCalledWith(true);

    await controller.refreshCapabilities();
    expect(onInitialAvailabilityResolved).toHaveBeenCalledOnce();
  });

  it("ignores an older capability response that arrives after a newer catalog", async () => {
    const older = deferred<ProChatCapabilities>();
    const newer = deferred<ProChatCapabilities>();
    const capabilityRequest = vi.fn()
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(newer.promise);
    const chatBridge = bridge({ capabilities: capabilityRequest });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(capabilityRequest).toHaveBeenCalledTimes(1));

    const latestRefresh = controller.refreshCapabilities();
    await vi.waitFor(() => expect(capabilityRequest).toHaveBeenCalledTimes(2));
    newer.resolve({
      providers: capabilities().providers.filter(
        ({ provider }) => provider === "anthropic",
      ),
    });
    await latestRefresh;
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value).toBe(
      JSON.stringify(["anthropic", "claude-current"]),
    );
    expect(chatBridge.history).toHaveBeenCalledWith("doc-1", "anthropic");

    older.resolve({
      providers: capabilities().providers.filter(
        ({ provider }) => provider === "openai",
      ),
    });
    await older.promise;
    await Promise.resolve();

    expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value).toBe(
      JSON.stringify(["anthropic", "claude-current"]),
    );
    expect(chatBridge.history).not.toHaveBeenCalledWith("doc-1", "openai");
  });

  it("discards late history after the document changes", async () => {
    const first = deferred<ProChatHistory>();
    const second = deferred<ProChatHistory>();
    const chatBridge = bridge({
      history: vi.fn().mockImplementation((documentId) =>
        documentId === "doc-1" ? first.promise : second.promise),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext("doc-1", "First"));
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-1", "openai"));
    controller.setDocument(documentContext("doc-2", "Second"));
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-2", "openai"));

    second.resolve(history("doc-2", "openai", [
      turn("turn-b", { user_text: "Second document", assistant_text: "B", status: "completed" }),
    ]));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Second document"));
    first.resolve(history("doc-1", "openai", [
      turn("turn-a", { user_text: "First document", assistant_text: "A", status: "completed" }),
    ]));
    await Promise.resolve();

    expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Second document");
    expect(document.querySelector("#pro-chat-messages")?.textContent).not.toContain("First document");
  });

  it("does not paint late stream events into a newly selected document", async () => {
    let onEvent: ((event: ProChatEvent) => void) | null = null;
    const activity = vi.fn();
    const chatBridge = bridge({
      start: vi.fn().mockImplementation(async (_request, callback) => {
        onEvent = callback;
        return { operation_id: "operation-1", turn_id: "turn-1" };
      }),
    });
    controller = installProChat(document, {
      bridge: chatBridge,
      onActivityChange: activity,
    });
    controller.setDocument(documentContext("doc-1", "First"));
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-1", "openai"));
    enableComposer();
    document.querySelector<HTMLButtonElement>("#pro-chat-send")!.click();
    await vi.waitFor(() => expect(chatBridge.start).toHaveBeenCalled());
    const pending = turn("turn-1", { user_text: "From the first document" });
    onEvent!({
      type: "started",
      operation_id: "operation-1",
      turn: pending,
      revision: 1,
    });

    controller.setDocument(documentContext("doc-2", "Second"));
    await vi.waitFor(() => expect(chatBridge.stop).toHaveBeenCalledWith("operation-1"));
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("First");
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-stop")!.hidden).toBe(false);
    onEvent!({
      type: "stopped",
      operation_id: "operation-1",
      turn: {
        ...pending,
        status: "stopped",
        retryable: true,
      },
      revision: 2,
    });
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-2", "openai"));
    onEvent!({
      type: "completed",
      operation_id: "operation-1",
      turn: {
        ...pending,
        assistant_text: "Late text",
        status: "completed",
      },
      revision: 2,
    });

    expect(document.querySelector("#pro-chat-messages")?.textContent).not.toContain("Late text");
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("Second");
    expect(activity).toHaveBeenLastCalledWith(false);
  });

  it("keeps the old response visible after Stop fails, then switches on a late terminal event", async () => {
    let onEvent: ((event: ProChatEvent) => void) | null = null;
    const chatBridge = bridge({
      start: vi.fn().mockImplementation(async (_request, callback) => {
        onEvent = callback;
        return { operation_id: "operation-1", turn_id: "turn-1" };
      }),
      stop: vi.fn().mockRejectedValue(new Error("temporary stop failure")),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext("doc-1", "First"));
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-1", "openai"));
    enableComposer();
    document.querySelector<HTMLButtonElement>("#pro-chat-send")!.click();
    await vi.waitFor(() => expect(chatBridge.start).toHaveBeenCalled());
    const pending = turn("turn-1", { user_text: "From the first document" });
    onEvent!({
      type: "started",
      operation_id: "operation-1",
      turn: pending,
      revision: 1,
    });

    controller.setDocument(documentContext("doc-2", "Second"));

    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent).toContain(
      "temporary stop failure",
    ));
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("First");
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-stop")!.hidden).toBe(false);
    expect(chatBridge.history).not.toHaveBeenCalledWith("doc-2", "openai");

    onEvent!({
      type: "completed",
      operation_id: "operation-1",
      turn: {
        ...pending,
        assistant_text: "Late completed answer",
        status: "completed",
      },
      revision: 2,
    });

    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-2", "openai"));
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("Second");
    expect(document.querySelector("#pro-chat-error")?.textContent).not.toContain(
      "temporary stop failure",
    );
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-stop")!.hidden).toBe(true);
  });

  it("does not move focus back from the editor when a response finishes", async () => {
    let onEvent: ((event: ProChatEvent) => void) | null = null;
    const chatBridge = bridge({
      start: vi.fn().mockImplementation(async (_request, callback) => {
        onEvent = callback;
        return { operation_id: "operation-1", turn_id: "turn-1" };
      }),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalled());
    enableComposer();
    document.querySelector<HTMLButtonElement>("#pro-chat-send")!.click();
    await vi.waitFor(() => expect(chatBridge.start).toHaveBeenCalled());
    const pending = turn("turn-1", { user_text: "Visible message" });
    onEvent!({
      type: "started",
      operation_id: "operation-1",
      turn: pending,
      revision: 1,
    });
    const editorControl = document.createElement("button");
    editorControl.textContent = "Editor control";
    document.body.append(editorControl);
    editorControl.focus();

    onEvent!({
      type: "completed",
      operation_id: "operation-1",
      turn: { ...pending, assistant_text: "Done", status: "completed" },
      revision: 2,
    });

    expect(document.activeElement).toBe(editorControl);
  });

  it("clears only the current document and provider conversation after confirmation", async () => {
    const existing = history("doc-1", "openai", [
      turn("turn-1", { assistant_text: "Answer", status: "completed" }),
    ], 8);
    const chatBridge = bridge({
      history: vi.fn().mockResolvedValue(existing),
      clear: vi.fn().mockResolvedValue(history("doc-1", "openai", [], 9)),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Answer"));

    document.querySelector<HTMLButtonElement>("#pro-chat-clear")!.click();
    const confirmation = document.querySelector<HTMLElement>("#pro-chat-clear-confirmation")!;
    expect(confirmation.hidden).toBe(false);
    expect(confirmation.getAttribute("role")).toBe("dialog");
    expect(confirmation.hasAttribute("aria-modal")).toBe(false);
    expect(confirmation.getAttribute("aria-labelledby")).toBe("pro-chat-clear-title");
    expect(confirmation.getAttribute("aria-describedby")).toBe("pro-chat-clear-description");
    expect(confirmation.textContent).toContain("document and reviewer history stay intact");
    await vi.waitFor(() => {
      expect(document.activeElement).toBe(
        document.querySelector<HTMLButtonElement>("#pro-chat-clear-cancel"),
      );
    });
    document.querySelector<HTMLButtonElement>("#pro-chat-clear-cancel")!.click();
    expect(confirmation.hidden).toBe(true);
    expect(document.activeElement).toBe(document.querySelector("#pro-chat-clear"));
    document.querySelector<HTMLButtonElement>("#pro-chat-clear")!.click();
    await vi.waitFor(() => {
      expect(document.activeElement).toBe(
        document.querySelector<HTMLButtonElement>("#pro-chat-clear-cancel"),
      );
    });
    document.querySelector<HTMLButtonElement>("#pro-chat-clear-confirm")!.click();

    await vi.waitFor(() => expect(chatBridge.clear).toHaveBeenCalledWith("doc-1", "openai", 8));
    await vi.waitFor(() => expect(confirmation.hidden).toBe(true));
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
  });

  it("reloads a stale conversation and requires Clear confirmation again", async () => {
    const firstTurns = [
      turn("turn-1", { assistant_text: "First answer", status: "completed" }),
    ];
    const updatedTurns = [
      ...firstTurns,
      turn("turn-2", {
        user_text: "From another window",
        assistant_text: "New answer",
        status: "completed",
      }),
    ];
    const historyRequest = vi.fn()
      .mockResolvedValueOnce(history("doc-1", "openai", firstTurns, 8))
      .mockResolvedValueOnce(history("doc-1", "openai", updatedTurns, 9));
    const clear = vi.fn()
      .mockRejectedValueOnce(new Error("This conversation changed. Reload it before clearing."))
      .mockResolvedValueOnce(history("doc-1", "openai", [], 10));
    const chatBridge = bridge({ history: historyRequest, clear });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("First answer"));

    document.querySelector<HTMLButtonElement>("#pro-chat-clear")!.click();
    document.querySelector<HTMLButtonElement>("#pro-chat-clear-confirm")!.click();

    await vi.waitFor(() => expect(historyRequest).toHaveBeenCalledTimes(2));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent).toContain(
      "Confirm Clear chat again",
    ));
    const confirmation = document.querySelector<HTMLElement>("#pro-chat-clear-confirmation")!;
    expect(confirmation.hidden).toBe(false);
    expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("New answer");
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-error-retry")!.hidden).toBe(true);
    await vi.waitFor(() => expect(
      document.querySelector<HTMLButtonElement>("#pro-chat-clear-confirm")!.disabled,
    ).toBe(false));
    document.querySelector<HTMLButtonElement>("#pro-chat-clear-confirm")!.click();

    await vi.waitFor(() => expect(clear).toHaveBeenCalledTimes(2));
    expect(clear.mock.calls[1]).toEqual(["doc-1", "openai", 9]);
    await vi.waitFor(() => expect(confirmation.hidden).toBe(true));
  });

  it("does not show an old clear failure after switching documents", async () => {
    const pendingClear = deferred<ProChatHistory>();
    const chatBridge = bridge({
      history: vi.fn().mockImplementation((documentId, provider) =>
        Promise.resolve(history(
          documentId,
          provider,
          documentId === "doc-1"
            ? [turn("turn-1", { assistant_text: "Answer", status: "completed" })]
            : [],
          documentId === "doc-1" ? 3 : 0,
        ))),
      clear: vi.fn().mockReturnValue(pendingClear.promise),
    });
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext("doc-1", "First"));
    controller.setActive(true);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Answer"));
    document.querySelector<HTMLButtonElement>("#pro-chat-clear")!.click();
    document.querySelector<HTMLButtonElement>("#pro-chat-clear-confirm")!.click();
    await vi.waitFor(() => expect(chatBridge.clear).toHaveBeenCalled());

    controller.setDocument(documentContext("doc-2", "Second"));
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith("doc-2", "openai"));
    pendingClear.reject(new Error("old clear failed"));
    await Promise.resolve();

    expect(document.querySelector("#pro-chat-error")?.textContent).not.toContain(
      "old clear failed",
    );
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("Second");
  });

  it("shows unsupported thinking levels without allowing them", async () => {
    const chatBridge = bridge();
    controller = installProChat(document, { bridge: chatBridge });
    controller.setDocument(documentContext());
    controller.setActive(true);
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalled());

    const choices = [...document.querySelectorAll<HTMLOptionElement>(
      "#pro-chat-thinking option",
    )];
    expect(choices.find(({ value }) => value === "medium")?.disabled).toBe(false);
    expect(choices.find(({ value }) => value === "max")?.disabled).toBe(true);
    expect(document.querySelector("#pro-chat-provider")).toBeNull();
    expect(document.querySelector("#pro-chat-selection-summary")?.textContent).toBe(
      "GPT Current · Provider default",
    );
    expect([...document.querySelectorAll<HTMLOptGroupElement>(
      "#pro-chat-model optgroup",
    )].map(({ label }) => label)).toEqual(["OpenAI", "Anthropic"]);

    const model = document.querySelector<HTMLSelectElement>("#pro-chat-model")!;
    model.value = JSON.stringify(["anthropic", "claude-current"]);
    model.dispatchEvent(new Event("change", { bubbles: true }));
    await vi.waitFor(() => expect(chatBridge.history).toHaveBeenCalledWith(
      "doc-1",
      "anthropic",
    ));
    const thinking = document.querySelector<HTMLSelectElement>("#pro-chat-thinking")!;
    thinking.value = "high";
    thinking.dispatchEvent(new Event("change", { bubbles: true }));
    expect(document.querySelector("#pro-chat-selection-summary")?.textContent).toBe(
      "Claude Current · High",
    );

    const controls = document.querySelector<HTMLDetailsElement>("#pro-chat-controls")!;
    controls.open = true;
    thinking.focus();
    controls.dispatchEvent(new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    }));
    expect(controls.open).toBe(false);
    expect(document.activeElement).toBe(controls.querySelector("summary"));
  });
});
