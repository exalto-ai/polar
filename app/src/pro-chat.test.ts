import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatSuggestionInput } from "./editor-api";
import type { ProChatBridge, SendChatRequest } from "./pro-chat-bridge";
import { installProChat, type ProChatDocument } from "./pro-chat";

type ChatOptions = NonNullable<Parameters<typeof installProChat>[1]>;
type ChatStorage = NonNullable<ChatOptions["storage"]>;

function memoryStorage(): ChatStorage {
  const values = new Map<string, string>();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => {
      values.delete(key);
    },
  };
}

let storage: ChatStorage;

function installChat(options: ChatOptions = {}) {
  return installProChat(document, { storage, ...options });
}

function markup(): string {
  return `
    <section id="pro-chat">
      <button id="pro-chat-new"></button>
      <p id="pro-chat-document"></p>
      <select id="pro-chat-provider"><option value=""></option><option value="openai">OpenAI</option></select>
      <select id="pro-chat-model"></select>
      <p id="pro-chat-error" hidden></p>
      <p id="pro-chat-storage-notice" hidden></p>
      <button id="pro-chat-retry" hidden></button>
      <p id="pro-chat-empty"></p>
      <ol id="pro-chat-messages" hidden></ol>
      <form id="pro-chat-form">
        <button id="pro-chat-focus-capture" type="button"></button>
        <div id="pro-chat-focus" hidden><span id="pro-chat-focus-text"></span><button id="pro-chat-focus-remove" type="button"></button></div>
        <input id="pro-chat-consent" type="checkbox" />
        <textarea id="pro-chat-input"></textarea>
        <button id="pro-chat-send" type="submit"></button>
      </form>
    </section>
  `;
}

function chatDocument(id = "private-document-id", title = "Draft"): ProChatDocument {
  return {
    id,
    title,
    snapshot: () => ({ type: "doc", content: [] }),
    suggestionPosition: () => ({ kind: "end" }),
    waitUntilSaved: async () => true,
    selectedText: () => null,
  };
}

function chatBridge(overrides: Partial<ProChatBridge> = {}): ProChatBridge {
  return {
    models: vi.fn().mockResolvedValue({
      provider: "openai",
      models: [
        { id: "gpt-test", display_name: "GPT Test" },
        { id: "gpt-second", display_name: "GPT Second" },
      ],
    }),
    send: vi.fn().mockResolvedValue({
      text: "Reply",
      provider: "openai",
      requested_model: "gpt-test",
      reported_model: null,
      wording_revision: "revision-1",
      complete: true,
    }),
    ...overrides,
  };
}

async function chooseOpenAi(bridge: ProChatBridge): Promise<void> {
  const provider = document.querySelector<HTMLSelectElement>("#pro-chat-provider")!;
  provider.value = "openai";
  provider.dispatchEvent(new Event("change"));
  await vi.waitFor(() => {
    expect(bridge.models).toHaveBeenCalledWith("openai");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value)
      .not.toBe("");
  });
  const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
  consent.checked = true;
  consent.dispatchEvent(new Event("change"));
}

function compose(message: string): void {
  const input = document.querySelector<HTMLTextAreaElement>("#pro-chat-input")!;
  input.value = message;
  input.dispatchEvent(new Event("input"));
}

function submitChat(): void {
  document.querySelector<HTMLFormElement>("#pro-chat-form")!
    .dispatchEvent(new Event("submit", { cancelable: true }));
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

beforeEach(() => {
  storage = memoryStorage();
  document.body.innerHTML = markup();
});

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("built-in chat", () => {
  it("shares the current snapshot only after the user acknowledges the notice", async () => {
    let sent: SendChatRequest | null = null;
    let suggested: ChatSuggestionInput | null = null;
    const bridge: ProChatBridge = {
      models: vi.fn().mockResolvedValue({
        provider: "openai",
        models: [{ id: "gpt-test", display_name: "GPT Test" }],
      }),
      send: vi.fn().mockImplementation(async (request: SendChatRequest) => {
        sent = request;
        return {
          text: "A clearer ending",
          provider: "openai",
          requested_model: "gpt-test",
          reported_model: "gpt-test-2026",
          wording_revision: "revision-1",
          complete: true,
        };
      }),
    };
    const controller = installChat({
      bridge,
      createRequestId: () => "suggestion-one",
      suggestResponse: vi.fn().mockImplementation(async (input: ChatSuggestionInput) => {
        suggested = input;
      }),
    });
    controller.setActive(true);
    controller.setDocument({
      id: "private-document-id",
      title: "Draft",
      snapshot: () => ({ type: "doc", content: [] }),
      suggestionPosition: () => ({ kind: "end" }),
      waitUntilSaved: async () => true,
      selectedText: () => "Selected line",
    });

    const provider = document.querySelector<HTMLSelectElement>("#pro-chat-provider")!;
    provider.value = "openai";
    provider.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(bridge.models).toHaveBeenCalledWith("openai");
      expect(bridge.models).toHaveBeenCalledTimes(1);
      expect([
        document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value,
        document.querySelector("#pro-chat-error")?.textContent,
      ]).toEqual(["gpt-test", ""]);
    });

    const input = document.querySelector<HTMLTextAreaElement>("#pro-chat-input")!;
    const send = document.querySelector<HTMLButtonElement>("#pro-chat-send")!;
    input.value = "Improve the ending";
    input.dispatchEvent(new Event("input"));
    expect(send.disabled).toBe(true);
    const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
    consent.checked = true;
    consent.dispatchEvent(new Event("change"));
    expect(send.disabled).toBe(false);
    document.querySelector<HTMLButtonElement>("#pro-chat-focus-capture")!.click();
    expect(document.querySelector("#pro-chat-focus")?.textContent).toContain("Selected line");

    document.querySelector<HTMLFormElement>("#pro-chat-form")!
      .dispatchEvent(new Event("submit", { cancelable: true }));
    await vi.waitFor(() => expect(sent).not.toBeNull());
    expect(sent).toMatchObject({
      document_title: "Draft",
      document: { type: "doc", content: [] },
      message: "Improve the ending",
      focus_text: "Selected line",
      disclosure_version: 1,
    });
    expect(sent).not.toHaveProperty("document_id");
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-messages")?.textContent)
        .toContain("A clearer ending");
    });
    document.querySelector<HTMLButtonElement>(".pro-chat-suggest")!.click();
    await vi.waitFor(() => expect(suggested).not.toBeNull());
    expect(suggested).toMatchObject({
      documentId: "private-document-id",
      requestId: "suggestion-one",
      provider: "openai",
      assistantText: "A clearer ending",
      wordingRevision: "revision-1",
      after: { kind: "end" },
    });
    controller.destroy();
  });

  it("persists a suggestion request ID before the call and reuses it after restart", async () => {
    const bridge = chatBridge({
      send: vi.fn().mockResolvedValue({
        text: "A clearer ending",
        provider: "openai",
        requested_model: "gpt-test",
        reported_model: "gpt-test-2026",
        wording_revision: "revision-1",
        complete: true,
      }),
    });
    const createRequestId = vi.fn()
      .mockReturnValueOnce("suggestion-one")
      .mockReturnValueOnce("suggestion-two");
    const suggestResponse = vi.fn()
      .mockImplementationOnce(async () => {
        const saved = storage.getItem(
          "thought.pro-chat.v1.private-document-id",
        );
        expect(saved).toContain('"suggestionRequestId":"suggestion-one"');
        throw new Error("response lost");
      })
      .mockResolvedValueOnce(undefined);
    let controller = installChat({
      bridge,
      createRequestId,
      suggestResponse,
    });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    await chooseOpenAi(bridge);
    compose("Improve the ending");
    submitChat();
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-messages")?.textContent)
        .toContain("A clearer ending");
    });

    document.querySelector<HTMLButtonElement>(".pro-chat-suggest")!.click();
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-error")?.textContent)
        .toContain("response lost");
    });

    const saved = JSON.parse(storage.getItem(
      "thought.pro-chat.v1.private-document-id",
    )!);
    expect(saved.messages[1].text).toBe("A clearer ending");
    expect(saved.messages[1].response).not.toHaveProperty("text");
    controller.destroy();
    document.body.innerHTML = markup();
    controller = installChat({
      bridge,
      createRequestId,
      suggestResponse,
    });
    controller.setDocument(chatDocument());
    expect(document.querySelector("#pro-chat-messages")?.textContent)
      .toContain("A clearer ending");
    controller.setActive(true);
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value)
        .toBe("gpt-test");
    });
    document.querySelector<HTMLButtonElement>(".pro-chat-suggest")!.click();
    await vi.waitFor(() => expect(suggestResponse).toHaveBeenCalledTimes(2));

    expect(suggestResponse.mock.calls.map(([request]) => request.requestId))
      .toEqual(["suggestion-one", "suggestion-one"]);
    expect(createRequestId).toHaveBeenCalledTimes(1);
    expect(document.querySelector("#pro-chat-messages")?.textContent)
      .toContain("Suggested");
    controller.destroy();
  });

  it("restores isolated per-document chat and model, while New chat clears one", async () => {
    const bridge = chatBridge();
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument("one", "One"));
    await chooseOpenAi(bridge);
    const model = document.querySelector<HTMLSelectElement>("#pro-chat-model")!;
    model.value = "gpt-second";
    model.dispatchEvent(new Event("change"));
    compose("Question");
    submitChat();
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Reply");
    });
    controller.setDocument(chatDocument("two", "Two"));
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("Two");
    await chooseOpenAi(bridge);
    await vi.waitFor(() => expect(storage.getItem("thought.pro-chat.v1.two"))
      .not.toBeNull());

    controller.setDocument(chatDocument("one", "One"));
    expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Reply");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-provider")!.value)
      .toBe("openai");
    await vi.waitFor(() => expect(document.querySelector<HTMLSelectElement>(
      "#pro-chat-model",
    )!.value).toBe("gpt-second"));

    document.querySelector<HTMLButtonElement>("#pro-chat-new")!.click();
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
    expect(storage.getItem("thought.pro-chat.v1.one")).toBeNull();
    expect(storage.getItem("thought.pro-chat.v1.two")).not.toBeNull();
    controller.destroy();
  });

  it("keeps live chat usable when storage fails and reports one concise notice", async () => {
    const failingStorage = {
      getItem: vi.fn().mockReturnValue(null),
      setItem: vi.fn().mockImplementation(() => {
        throw new Error("quota unavailable");
      }),
      removeItem: vi.fn().mockImplementation(() => {
        throw new Error("quota unavailable");
      }),
    };
    const bridge = chatBridge();
    const controller = installChat({ bridge, storage: failingStorage });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    await chooseOpenAi(bridge);
    expect(document.querySelector<HTMLElement>("#pro-chat-storage-notice")!.hidden)
      .toBe(false);
    expect(document.querySelector("#pro-chat-storage-notice")?.textContent)
      .toBe("Saved chat is unavailable. This chat will continue in this window.");

    compose("Still works");
    submitChat();
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-messages")?.textContent)
      .toContain("Reply"));
    document.querySelector<HTMLButtonElement>("#pro-chat-new")!.click();
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
    expect(document.querySelectorAll("#pro-chat-storage-notice")).toHaveLength(1);
    controller.destroy();
  });

  it("preserves the last good saved history when the next record is too large", async () => {
    const assistantText = "A".repeat(64 * 1024);
    const bridge = chatBridge({
      send: vi.fn().mockResolvedValue({
        text: assistantText,
        provider: "openai",
        requested_model: "gpt-test",
        reported_model: null,
        wording_revision: "revision-large",
        complete: true,
      }),
    });
    let request = 0;
    const controller = installChat({
      bridge,
      createRequestId: () => `suggestion-${++request}`,
    });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    await chooseOpenAi(bridge);

    for (let index = 0; index < 9; index += 1) {
      compose(`Question ${index}`);
      submitChat();
      await vi.waitFor(() => expect(document.querySelectorAll(
        '#pro-chat-messages li[data-role="assistant"]',
      )).toHaveLength(index + 1));
    }
    const key = "thought.pro-chat.v1.private-document-id";
    const lastGood = storage.getItem(key);
    expect(lastGood).not.toBeNull();

    compose("Question 9");
    submitChat();
    await vi.waitFor(() => expect(document.querySelectorAll(
      '#pro-chat-messages li[data-role="assistant"]',
    )).toHaveLength(10));
    expect(document.querySelector<HTMLElement>("#pro-chat-storage-notice")!.hidden)
      .toBe(false);
    expect(storage.getItem(key)).toBe(lastGood);
    controller.destroy();
  });

  it("clears invalidated model-loading state when the document changes", async () => {
    const models = deferred<Awaited<ReturnType<ProChatBridge["models"]>>>();
    const bridge = chatBridge({ models: vi.fn().mockReturnValue(models.promise) });
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument("one", "One"));
    const provider = document.querySelector<HTMLSelectElement>("#pro-chat-provider")!;
    provider.value = "openai";
    provider.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat")?.getAttribute(
      "aria-busy",
    )).toBe("true"));

    controller.setDocument(chatDocument("two", "Two"));
    expect(document.querySelector("#pro-chat")?.getAttribute("aria-busy")).toBe("false");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.disabled).toBe(true);
    models.resolve({
      provider: "openai",
      models: [{ id: "stale-model", display_name: "Stale model" }],
    });
    await models.promise;
    await Promise.resolve();
    expect(document.querySelector("#pro-chat")?.getAttribute("aria-busy")).toBe("false");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.options)
      .toHaveLength(0);
    controller.destroy();
  });

  it("clears pending model-loading state when the provider is cleared", async () => {
    const models = deferred<Awaited<ReturnType<ProChatBridge["models"]>>>();
    const bridge = chatBridge({ models: vi.fn().mockReturnValue(models.promise) });
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    const provider = document.querySelector<HTMLSelectElement>("#pro-chat-provider")!;
    provider.value = "openai";
    provider.dispatchEvent(new Event("change"));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat")?.getAttribute(
      "aria-busy",
    )).toBe("true"));

    provider.value = "";
    provider.dispatchEvent(new Event("change"));
    expect(document.querySelector("#pro-chat")?.getAttribute("aria-busy")).toBe("false");
    models.resolve({
      provider: "openai",
      models: [{ id: "stale-model", display_name: "Stale model" }],
    });
    await models.promise;
    await Promise.resolve();
    expect(document.querySelector("#pro-chat")?.getAttribute("aria-busy")).toBe("false");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-model")!.options)
      .toHaveLength(0);
    controller.destroy();
  });

  it("disables restored conversation actions while models reload", async () => {
    const key = "thought.pro-chat.v1.private-document-id";
    storage.setItem(key, JSON.stringify({
      version: 1,
      provider: "openai",
      model: "gpt-test",
      messages: [
        { role: "user", text: "Question" },
        {
          role: "assistant",
          text: "Answer",
          response: {
            provider: "openai",
            requested_model: "gpt-test",
            reported_model: null,
            wording_revision: "revision-1",
            complete: true,
          },
          suggestionRequestId: "suggestion-one",
        },
      ],
    }));
    const models = deferred<Awaited<ReturnType<ProChatBridge["models"]>>>();
    const controller = installChat({
      bridge: chatBridge({ models: vi.fn().mockReturnValue(models.promise) }),
      suggestResponse: vi.fn(),
    });
    controller.setDocument(chatDocument());
    controller.setActive(true);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat")?.getAttribute(
      "aria-busy",
    )).toBe("true"));
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-new")!.disabled).toBe(true);
    expect(document.querySelector<HTMLButtonElement>(".pro-chat-suggest")!.disabled)
      .toBe(true);

    models.resolve({
      provider: "openai",
      models: [{ id: "gpt-test", display_name: "GPT Test" }],
    });
    await vi.waitFor(() => expect(document.querySelector("#pro-chat")?.getAttribute(
      "aria-busy",
    )).toBe("false"));
    expect(document.querySelector<HTMLButtonElement>("#pro-chat-new")!.disabled).toBe(false);
    expect(document.querySelector<HTMLButtonElement>(".pro-chat-suggest")!.disabled)
      .toBe(false);
    controller.destroy();
  });

  it("removes incomplete or unsafe saved history", () => {
    const key = "thought.pro-chat.v1.private-document-id";
    const response = {
      provider: "openai",
      requested_model: "gpt-test",
      reported_model: null,
      wording_revision: "revision-1",
      complete: true,
    };
    const malformed = [
      [{ role: "user", text: "Dangling user turn" }],
      [{ role: "user", text: "Question" }, { role: "assistant", text: "No metadata" }],
      [
        { role: "user", text: "Question" },
        {
          role: "assistant",
          text: "Unsafe retry ID",
          response,
          suggestionRequestId: "contains spaces",
        },
      ],
      [
        { role: "user", text: "Question" },
        {
          role: "assistant",
          text: "Unsafe response metadata",
          response: { ...response, requested_model: "model\u0085name" },
          suggestionRequestId: "suggestion-safe",
        },
      ],
    ];
    for (const messages of malformed) {
      document.body.innerHTML = markup();
      storage.setItem(key, JSON.stringify({
        version: 1,
        provider: "openai",
        model: "gpt-test",
        messages,
      }));
      const controller = installChat({ bridge: chatBridge() });
      controller.setDocument(chatDocument());
      expect(storage.getItem(key)).toBeNull();
      expect(document.querySelector<HTMLElement>("#pro-chat-storage-notice")!.hidden)
        .toBe(false);
      controller.destroy();
    }
  });

  it("blocks a full conversation without truncating saved history", async () => {
    const key = "thought.pro-chat.v1.private-document-id";

    const messages = Array.from({ length: 30 }, (_, index) => index % 2 === 0
      ? { role: "user", text: `Question ${index / 2}` }
      : {
          role: "assistant",
          text: `Answer ${(index - 1) / 2}`,
          response: {
            provider: "openai",
            requested_model: "gpt-test",
            reported_model: null,
            wording_revision: `revision-${index}`,
            complete: true,
          },
          suggestionRequestId: `suggestion-${index}`,
        });
    storage.setItem(key, JSON.stringify({
      version: 1,
      provider: "openai",
      model: "gpt-test",
      messages,
    }));
    const bridge = chatBridge();
    const controller = installChat({ bridge });
    controller.setDocument(chatDocument());
    controller.setActive(true);
    await vi.waitFor(() => expect(document.querySelector<HTMLSelectElement>(
      "#pro-chat-model",
    )!.value).toBe("gpt-test"));
    const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
    consent.checked = true;
    consent.dispatchEvent(new Event("change"));
    compose("One too many");
    submitChat();
    expect(document.querySelector("#pro-chat-error")?.textContent)
      .toBe("This conversation is full. Start a new chat.");
    expect(bridge.send).not.toHaveBeenCalled();
    controller.destroy();
  });
});
