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
      <select id="pro-chat-thinking">
        <option value="provider_default">Provider default</option>
        <option value="low">Low</option>
        <option value="medium">Medium</option>
        <option value="high">High</option>
      </select>
      <p id="pro-chat-error" hidden></p>
      <p id="pro-chat-storage-notice" hidden></p>
      <button id="pro-chat-retry" hidden></button>
      <p id="pro-chat-empty"></p>
      <ol id="pro-chat-messages" hidden></ol>
      <form id="pro-chat-form">
        <button id="pro-chat-focus-capture" type="button"></button>
        <div id="pro-chat-focus" hidden><span id="pro-chat-focus-text"></span><button id="pro-chat-focus-remove" type="button"></button></div>
        <button id="pro-chat-attach" type="button"></button>
        <input id="pro-chat-attachment-input" type="file" multiple />
        <ul id="pro-chat-attachments" hidden></ul>
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

function file(
  name: string,
  type: string,
  bytes: Uint8Array,
  declaredSize = bytes.byteLength,
): File {
  return {
    name,
    type,
    size: declaredSize,
    arrayBuffer: vi.fn().mockResolvedValue(
      bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    ),
  } as unknown as File;
}

function selectFiles(files: File[]): void {
  const input = document.querySelector<HTMLInputElement>("#pro-chat-attachment-input")!;
  Object.defineProperty(input, "files", { configurable: true, value: files });
  input.dispatchEvent(new Event("change"));
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
  it("shares the current snapshot with the current disclosure contract", async () => {
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
      thinking: "provider_default",
      attachments: [],
      disclosure_version: 2,
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

  it("restores isolated per-document chat and settings, while New chat clears one", async () => {
    const bridge = chatBridge();
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument("one", "One"));
    await chooseOpenAi(bridge);
    const model = document.querySelector<HTMLSelectElement>("#pro-chat-model")!;
    model.value = "gpt-second";
    model.dispatchEvent(new Event("change"));
    const thinking = document.querySelector<HTMLSelectElement>("#pro-chat-thinking")!;
    thinking.value = "high";
    thinking.dispatchEvent(new Event("change"));
    compose("Question");
    submitChat();
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Reply");
    });
    expect(document.querySelector("#pro-chat-messages")?.textContent)
      .toContain("High thinking requested");

    controller.setDocument(chatDocument("two", "Two"));
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("Two");
    await chooseOpenAi(bridge);
    await vi.waitFor(() => expect(storage.getItem("thought.pro-chat.v1.two"))
      .not.toBeNull());

    controller.setDocument(chatDocument("one", "One"));
    expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Reply");
    expect(document.querySelector("#pro-chat-messages")?.textContent)
      .toContain("High thinking requested");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-provider")!.value)
      .toBe("openai");
    expect(document.querySelector<HTMLSelectElement>("#pro-chat-thinking")!.value).toBe("high");
    await vi.waitFor(() => expect(document.querySelector<HTMLSelectElement>(
      "#pro-chat-model",
    )!.value).toBe("gpt-second"));

    document.querySelector<HTMLButtonElement>("#pro-chat-new")!.click();
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
    expect(storage.getItem("thought.pro-chat.v1.one")).toBeNull();
    expect(storage.getItem("thought.pro-chat.v1.two")).not.toBeNull();
    controller.destroy();
  });

  it("sends validated files once, persists only summaries, and records thinking", async () => {
    const bridge = chatBridge();
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    await chooseOpenAi(bridge);
    const thinking = document.querySelector<HTMLSelectElement>("#pro-chat-thinking")!;
    thinking.value = "medium";
    thinking.dispatchEvent(new Event("change"));

    const pdf = new TextEncoder().encode("%PDF-1.7\nprivate pdf bytes");
    const text = new TextEncoder().encode("private text contents");
    selectFiles([
      file("brief.pdf", "application/pdf", pdf),
      file("notes.md", "text/markdown", text),
    ]);
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-attachments")?.textContent)
        .toContain("brief.pdf");
      expect(document.querySelector("#pro-chat-attachments")?.textContent)
        .toContain("notes.md");
    });
    expect(storage.getItem("thought.pro-chat.v1.private-document-id"))
      .not.toContain("private text contents");

    compose("Use these files");
    submitChat();
    await vi.waitFor(() => expect(bridge.send).toHaveBeenCalledTimes(1));
    expect(vi.mocked(bridge.send).mock.calls[0][0]).toMatchObject({
      thinking: "medium",
      disclosure_version: 2,
      attachments: [
        {
          name: "brief.pdf",
          media_type: "application/pdf",
          content_base64: btoa(String.fromCharCode(...pdf)),
        },
        {
          name: "notes.md",
          media_type: "text/plain",
          content_base64: btoa(String.fromCharCode(...text)),
        },
      ],
    });
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-messages")?.textContent)
      .toContain("Medium thinking requested"));
    expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("brief.pdf");
    expect(document.querySelector("#pro-chat-attachments")?.textContent).toBe("");

    const saved = storage.getItem("thought.pro-chat.v1.private-document-id")!;
    expect(saved).toContain('"name":"brief.pdf"');
    expect(saved).toContain('"size_bytes":26');
    expect(saved).not.toContain("content_base64");
    expect(saved).not.toContain("private pdf bytes");
    expect(saved).not.toContain("private text contents");

    compose("Follow up");
    submitChat();
    await vi.waitFor(() => expect(bridge.send).toHaveBeenCalledTimes(2));
    expect(vi.mocked(bridge.send).mock.calls[1][0].attachments).toEqual([]);
    expect(vi.mocked(bridge.send).mock.calls[1][0].messages)
      .toEqual([
        { role: "user", text: "Use these files" },
        { role: "assistant", text: "Reply" },
      ]);
    controller.destroy();
  });

  it("keeps the typed message and staged files when provider delivery fails", async () => {
    const send = vi.fn()
      .mockRejectedValueOnce(new Error("network unavailable"))
      .mockResolvedValueOnce({
        text: "Recovered",
        provider: "openai",
        requested_model: "gpt-test",
        reported_model: null,
        wording_revision: "revision-2",
        complete: true,
      });
    const bridge = chatBridge({ send });
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    await chooseOpenAi(bridge);
    selectFiles([
      file("notes.txt", "text/plain", new TextEncoder().encode("retry me")),
    ]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-attachments")?.textContent)
      .toContain("notes.txt"));
    compose("Try once");
    submitChat();
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("network unavailable"));
    expect(document.querySelector<HTMLTextAreaElement>("#pro-chat-input")!.value)
      .toBe("Try once");
    expect(document.querySelector("#pro-chat-attachments")?.textContent)
      .toContain("notes.txt");

    submitChat();
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(2));
    expect(send.mock.calls[1][0].attachments).toHaveLength(1);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-attachments")?.textContent)
      .toBe(""));
    controller.destroy();
  });

  it("rejects unsupported, duplicate, invalid UTF-8, oversized, and excess files", async () => {
    const bridge = chatBridge();
    const controller = installChat({ bridge });
    controller.setActive(true);
    controller.setDocument(chatDocument());
    await chooseOpenAi(bridge);

    selectFiles(Array.from({ length: 6 }, (_, index) =>
      file(`${index}.txt`, "text/plain", new Uint8Array([65]))));
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("no more than 5"));

    selectFiles([
      file("large.txt", "text/plain", new Uint8Array([65]), 512 * 1024 + 1),
    ]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("512 KiB"));
    selectFiles([
      file("large.pdf", "application/pdf", new Uint8Array([37, 80, 68, 70, 45]),
        10 * 1024 * 1024 + 1),
    ]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("10 MiB"));

    selectFiles([file("bad.txt", "text/plain", new Uint8Array([0xc3, 0x28]))]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("not a UTF-8"));
    selectFiles([file("archive.zip", "application/zip", new Uint8Array([65]))]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("Only PDF and UTF-8"));
    selectFiles([file("folder/notes.txt", "text/plain", new Uint8Array([65]))]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("cannot contain paths"));

    selectFiles([file("notes.txt", "text/plain", new Uint8Array([65]))]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-attachments")?.textContent)
      .toContain("notes.txt"));
    selectFiles([file(" notes.txt ", "text/plain", new Uint8Array([66]))]);
    await vi.waitFor(() => expect(document.querySelector("#pro-chat-error")?.textContent)
      .toContain("already attached"));
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
      thinking: "medium",
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
          thinking: "medium",
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
          thinking: "medium",
          suggestionRequestId: "contains spaces",
        },
      ],
      [
        { role: "user", text: "Question" },
        {
          role: "assistant",
          text: "Unsafe response metadata",
          response: { ...response, requested_model: "model\u0085name" },
          thinking: "medium",
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
        thinking: "medium",
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
          thinking: "provider_default",
          suggestionRequestId: `suggestion-${index}`,
        });
    storage.setItem(key, JSON.stringify({
      version: 1,
      provider: "openai",
      model: "gpt-test",
      thinking: "provider_default",
      messages,
    }));
    const bridge = chatBridge();
    const controller = installChat({ bridge });
    controller.setDocument(chatDocument());
    controller.setActive(true);
    await vi.waitFor(() => expect(document.querySelector<HTMLSelectElement>(
      "#pro-chat-model",
    )!.value).toBe("gpt-test"));
    compose("One too many");
    submitChat();
    expect(document.querySelector("#pro-chat-error")?.textContent)
      .toBe("This conversation is full. Start a new chat.");
    expect(bridge.send).not.toHaveBeenCalled();
    controller.destroy();
  });
});
