import type {
  ChatMessage,
  ProChatBridge,
  ProviderModel,
  SendChatResponse,
} from "./pro-chat-bridge";
import type { ChatSuggestionInput } from "./editor-api";
import type { ProProvider } from "./pro-provider-bridge";
import type { SuggestionPosition } from "./suggestions";

const PROVIDER_NAMES: Record<ProProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};
const MAX_FOCUS_BYTES = 32 * 1024;
const MAX_PERSISTED_MESSAGES = 30;
const MAX_PERSISTED_RECORD_BYTES = 640 * 1024;
const MAX_PERSISTED_USER_MESSAGE_BYTES = 16 * 1024;
const MAX_PERSISTED_ASSISTANT_MESSAGE_BYTES = 64 * 1024;
const MAX_PERSISTED_IDENTIFIER_BYTES = 512;
const MAX_SUGGESTION_METADATA_BYTES = 160;
const MAX_SUGGESTION_REQUEST_ID_BYTES = 128;
const STORAGE_PREFIX = "thought.pro-chat.v1.";
const STORAGE_VERSION = 1;

export type ProChatDocument = {
  id: string;
  title: string;
  snapshot(): unknown;
  suggestionPosition(): SuggestionPosition;
  waitUntilSaved(): Promise<boolean>;
  selectedText(): string | null;
};

type ChatStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

type Options = {
  bridge?: ProChatBridge | null;
  suggestResponse?: (input: ChatSuggestionInput) => Promise<unknown>;
  createRequestId?: () => string;
  onNotice?: (message: string, kind?: "info" | "error") => void;
  storage?: ChatStorage | null;
};

type LocalMessage = ChatMessage & {
  meta?: string;
  incomplete?: boolean;
  response?: SendChatResponse;
  suggestionRequestId?: string;
  suggested?: boolean;
};

type PersistedResponse = Omit<SendChatResponse, "text">;

type PersistedMessage = ChatMessage & {
  response?: PersistedResponse;
  suggestionRequestId?: string;
  suggested?: true;
};

type PersistedConversation = {
  version: typeof STORAGE_VERSION;
  provider: ProProvider | null;
  model: string;
  messages: PersistedMessage[];
};

type RestoredConversation = Omit<PersistedConversation, "messages"> & {
  messages: LocalMessage[];
};

export type ProChatController = {
  setActive(active: boolean): void;
  setDocument(document: ProChatDocument | null): void;
  destroy(): void;
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing chat element: ${selector}`);
  return value;
}

function oneLine(error: unknown): string {
  const value = error instanceof Error ? error.message : String(error);
  return value.replace(/[\r\n\t]+/g, " ").trim().slice(0, 180) ||
    "The provider request failed.";
}

function provider(value: unknown): ProProvider | null {
  return value === "openai" || value === "anthropic" ? value : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function withinBytes(value: unknown, maximum: number): value is string {
  return typeof value === "string" && new TextEncoder().encode(value).byteLength <= maximum;
}

function storedString(value: unknown, maximum: number): value is string {
  return withinBytes(value, maximum) && value.trim().length > 0 && !value.includes("\0");
}

function validSuggestionRequestId(value: unknown): value is string {
  return typeof value === "string" && value.length <= MAX_SUGGESTION_REQUEST_ID_BYTES &&
    /^[A-Za-z0-9._-]+$/.test(value);
}

function validSuggestionMetadata(value: unknown): value is string {
  return storedString(value, MAX_SUGGESTION_METADATA_BYTES) &&
    !/[\u0000-\u001f\u007f-\u009f]/.test(value);
}

function response(value: unknown, text: string): SendChatResponse | undefined {
  if (!isRecord(value)) return undefined;
  const responseProvider = provider(value.provider);
  if (
    responseProvider === null ||
    !validSuggestionMetadata(value.requested_model) ||
    !(value.reported_model === null ||
      validSuggestionMetadata(value.reported_model)) ||
    !validSuggestionMetadata(value.wording_revision) ||
    typeof value.complete !== "boolean"
  ) return undefined;
  return {
    text,
    provider: responseProvider,
    requested_model: value.requested_model,
    reported_model: value.reported_model,
    wording_revision: value.wording_revision,
    complete: value.complete,
  };
}

function localMessage(value: unknown): LocalMessage | null {
  if (
    !isRecord(value) || (value.role !== "user" && value.role !== "assistant") ||
    !storedString(
      value.text,
      value.role === "user"
        ? MAX_PERSISTED_USER_MESSAGE_BYTES
        : MAX_PERSISTED_ASSISTANT_MESSAGE_BYTES,
    )
  ) return null;
  const savedResponse = response(value.response, value.text);
  if (value.response !== undefined && savedResponse === undefined) return null;
  const suggestionRequestId = validSuggestionRequestId(value.suggestionRequestId)
    ? value.suggestionRequestId
    : undefined;
  if (value.suggestionRequestId !== undefined && suggestionRequestId === undefined) return null;
  if (value.suggested !== undefined && value.suggested !== true) return null;
  if (
    (value.role === "assistant" && savedResponse === undefined) ||
    (savedResponse !== undefined && value.role !== "assistant")
  ) return null;
  if (savedResponse && suggestionRequestId === undefined) return null;
  const hasAssistantOnlyState = value.response !== undefined ||
    value.suggestionRequestId !== undefined || value.suggested !== undefined;
  if (
    (value.role !== "assistant" && hasAssistantOnlyState) ||
    (value.role === "assistant" && hasAssistantOnlyState && savedResponse === undefined) ||
    (value.suggested === true && suggestionRequestId === undefined)
  ) return null;
  const reportedModel = savedResponse?.reported_model ?? savedResponse?.requested_model;
  return {
    role: value.role,
    text: value.text,
    response: savedResponse,
    suggestionRequestId,
    suggested: value.suggested === true,
    meta: savedResponse
      ? `${PROVIDER_NAMES[savedResponse.provider]} · ${reportedModel}`
      : undefined,
    incomplete: savedResponse ? !savedResponse.complete : undefined,
  };
}

function persistedConversation(value: unknown): RestoredConversation | null {
  if (
    !isRecord(value) || value.version !== STORAGE_VERSION ||
    !Array.isArray(value.messages) || value.messages.length > MAX_PERSISTED_MESSAGES ||
    !withinBytes(value.model, MAX_PERSISTED_IDENTIFIER_BYTES) || value.model.includes("\0")
  ) return null;
  const savedProvider = value.provider === null ? null : provider(value.provider);
  if (value.provider !== null && savedProvider === null) return null;
  if (
    (savedProvider === null && value.model !== "") ||
    (savedProvider !== null && !storedString(value.model, MAX_PERSISTED_IDENTIFIER_BYTES))
  ) return null;
  const savedMessages = value.messages.map(localMessage);
  if (savedMessages.some((message) => message === null)) return null;
  if (savedMessages.length % 2 !== 0) return null;
  if (savedMessages.some((message, index) =>
    message?.role !== (index % 2 === 0 ? "user" : "assistant"))) return null;
  return {
    version: STORAGE_VERSION,
    provider: savedProvider,
    model: value.model,
    messages: savedMessages as LocalMessage[],
  };
}

function persistedMessage(message: LocalMessage): PersistedMessage {
  const saved: PersistedMessage = { role: message.role, text: message.text };
  if (message.response) {
    saved.response = {
      provider: message.response.provider,
      requested_model: message.response.requested_model,
      reported_model: message.response.reported_model,
      wording_revision: message.response.wording_revision,
      complete: message.response.complete,
    };
  }
  if (message.suggestionRequestId) saved.suggestionRequestId = message.suggestionRequestId;
  if (message.suggested) saved.suggested = true;
  return saved;
}

function storageKey(documentId: string): string {
  return `${STORAGE_PREFIX}${encodeURIComponent(documentId)}`;
}

export function installProChat(
  root: Document,
  options: Options = {},
): ProChatController {
  const panel = required<HTMLElement>(root, "#pro-chat");
  const providerSelect = required<HTMLSelectElement>(panel, "#pro-chat-provider");
  const modelSelect = required<HTMLSelectElement>(panel, "#pro-chat-model");
  const retry = required<HTMLButtonElement>(panel, "#pro-chat-retry");
  const storageNotice = required<HTMLElement>(panel, "#pro-chat-storage-notice");
  const notice = required<HTMLInputElement>(panel, "#pro-chat-consent");
  const documentLabel = required<HTMLElement>(panel, "#pro-chat-document");
  const messagesElement = required<HTMLOListElement>(panel, "#pro-chat-messages");
  const empty = required<HTMLElement>(panel, "#pro-chat-empty");
  const error = required<HTMLElement>(panel, "#pro-chat-error");
  const form = required<HTMLFormElement>(panel, "#pro-chat-form");
  const input = required<HTMLTextAreaElement>(panel, "#pro-chat-input");
  const send = required<HTMLButtonElement>(panel, "#pro-chat-send");
  const newChat = required<HTMLButtonElement>(panel, "#pro-chat-new");
  const captureFocus = required<HTMLButtonElement>(panel, "#pro-chat-focus-capture");
  const focus = required<HTMLElement>(panel, "#pro-chat-focus");
  const focusLabel = required<HTMLElement>(panel, "#pro-chat-focus-text");
  const removeFocus = required<HTMLButtonElement>(panel, "#pro-chat-focus-remove");
  const bridge = options.bridge ?? null;
  const createRequestId = options.createRequestId ?? (() => crypto.randomUUID());
  const disposers: Array<() => void> = [];
  let currentDocument: ProChatDocument | null = null;
  let messages: LocalMessage[] = [];
  let pendingText: string | null = null;
  let suggesting: LocalMessage | null = null;
  let focusText: string | null = null;
  let preferredModel = "";
  let active = false;
  let loadingModels = false;
  let destroyed = false;
  let requestGeneration = 0;
  let storageNoticeShown = false;
  let storage: ChatStorage | null = null;
  let storageAccessFailed = false;

  try {
    storage = options.storage === undefined ? window.localStorage : options.storage;
  } catch {
    storageAccessFailed = true;
  }

  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    event: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ) {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function setError(message: string | null): void {
    error.textContent = message ?? "";
    error.hidden = message === null;
  }

  function showStorageNotice(): void {
    if (storageNoticeShown) return;
    storageNoticeShown = true;
    storageNotice.textContent =
      "Saved chat is unavailable. This chat will continue in this window.";
    storageNotice.hidden = false;
  }

  if (storageAccessFailed || storage === null) showStorageNotice();

  function discardStoredConversation(documentId: string): void {
    if (storage === null) return;
    try {
      storage.removeItem(storageKey(documentId));
    } catch {
      // The single storage notice below covers both the original failure and
      // a best-effort cleanup failure. Live chat remains available.
    }
  }

  function readConversation(documentId: string): RestoredConversation | null {
    if (storage === null) return null;
    try {
      const raw = storage.getItem(storageKey(documentId));
      if (raw === null) return null;
      if (new TextEncoder().encode(raw).byteLength > MAX_PERSISTED_RECORD_BYTES) {
        discardStoredConversation(documentId);
        showStorageNotice();
        return null;
      }
      const saved = persistedConversation(JSON.parse(raw));
      if (saved === null) {
        discardStoredConversation(documentId);
        showStorageNotice();
      }
      return saved;
    } catch {
      discardStoredConversation(documentId);
      showStorageNotice();
      return null;
    }
  }

  function saveConversation(): void {
    if (storage === null || currentDocument === null) return;
    const saved: PersistedConversation = {
      version: STORAGE_VERSION,
      provider: provider(providerSelect.value),
      model: modelSelect.value || preferredModel,
      messages: messages.map(persistedMessage),
    };
    try {
      const serialized = JSON.stringify(saved);
      if (
        persistedConversation(saved) === null ||
        new TextEncoder().encode(serialized).byteLength > MAX_PERSISTED_RECORD_BYTES
      ) {
        showStorageNotice();
        return;
      }
      storage.setItem(storageKey(currentDocument.id), serialized);
    } catch {
      showStorageNotice();
    }
  }

  function clearSavedConversation(): void {
    if (storage === null || currentDocument === null) return;
    try {
      storage.removeItem(storageKey(currentDocument.id));
    } catch {
      showStorageNotice();
    }
  }

  function messageElement(message: LocalMessage, pending = false): HTMLLIElement {
    const item = root.createElement("li");
    item.dataset.role = message.role;
    if (pending) item.dataset.pending = "true";
    const label = root.createElement("strong");
    label.textContent = message.role === "user" ? "You" : "Assistant";
    const text = root.createElement("p");
    text.textContent = message.text;
    item.append(label, text);
    if (message.meta || message.incomplete) {
      const meta = root.createElement("small");
      meta.textContent = [
        message.meta,
        message.incomplete ? "Provider marked this response incomplete" : null,
      ].filter(Boolean).join(" · ");
      item.append(meta);
    }
    if (message.response?.complete && options.suggestResponse) {
      const suggest = root.createElement("button");
      suggest.type = "button";
      suggest.className = "text-button pro-chat-suggest";
      suggest.textContent = message.suggested
        ? "Suggested"
        : suggesting === message
          ? "Suggesting…"
          : "Suggest in document";
      suggest.disabled = message.suggested === true || loadingModels || suggesting !== null ||
        pendingText !== null;
      suggest.addEventListener("click", () => void suggestMessage(message));
      item.append(suggest);
    }
    return item;
  }

  function renderMessages(): void {
    const rendered = messages.map((message) => messageElement(message));
    if (pendingText !== null) {
      rendered.push(messageElement({ role: "user", text: pendingText }, true));
    }
    messagesElement.replaceChildren(...rendered);
    messagesElement.hidden = rendered.length === 0;
    empty.hidden = rendered.length !== 0;
    newChat.hidden = messages.length === 0 && pendingText === null;
    newChat.disabled = loadingModels || pendingText !== null || suggesting !== null;
  }

  function renderControls(): void {
    const selectedProvider = provider(providerSelect.value);
    const hasModel = modelSelect.value !== "";
    const busy = pendingText !== null || suggesting !== null;
    documentLabel.textContent = currentDocument
      ? `Current document: ${currentDocument.title}`
      : "Open a document to start a chat.";
    focus.hidden = focusText === null;
    focusLabel.textContent = focusText === null
      ? ""
      : focusText.length > 180
        ? `${focusText.slice(0, 177)}…`
        : focusText;
    providerSelect.disabled = busy;
    captureFocus.disabled = currentDocument === null || busy;
    modelSelect.disabled = selectedProvider === null || loadingModels || busy;
    retry.disabled = loadingModels || selectedProvider === null || bridge === null || busy;
    input.disabled = currentDocument === null || busy || bridge === null;
    send.disabled = currentDocument === null || selectedProvider === null || !hasModel ||
      !notice.checked || busy || input.value.trim() === "" || bridge === null;
    panel.setAttribute("aria-busy", String(loadingModels || busy));
    send.textContent = pendingText === null ? "Send" : "Sending…";
  }

  function render(): void {
    renderMessages();
    renderControls();
  }

  function clearTransientState(): void {
    requestGeneration += 1;
    loadingModels = false;
    retry.hidden = true;
    pendingText = null;
    suggesting = null;
    focusText = null;
    input.value = "";
    setError(null);
  }

  function newConversation(): void {
    clearTransientState();
    messages = [];
    clearSavedConversation();
    render();
  }

  function replaceModels(models: ProviderModel[], preferred = ""): void {
    const options = models.map((model) => {
      const option = root.createElement("option");
      option.value = model.id;
      option.textContent = model.display_name;
      return option;
    });
    modelSelect.replaceChildren(...options);
    modelSelect.value = models.some(({ id }) => id === preferred)
      ? preferred
      : models[0]?.id ?? "";
    preferredModel = modelSelect.value;
  }

  async function loadModels(): Promise<void> {
    const selectedProvider = provider(providerSelect.value);
    const wantedModel = preferredModel || modelSelect.value;
    const generation = ++requestGeneration;
    replaceModels([], wantedModel);
    preferredModel = wantedModel;
    retry.hidden = true;
    setError(null);
    if (selectedProvider === null || bridge === null || !active) {
      loadingModels = false;
      render();
      return;
    }
    loadingModels = true;
    render();
    try {
      const result = await bridge.models(selectedProvider);
      if (destroyed || generation !== requestGeneration) return;
      if (result.provider !== selectedProvider || result.models.length === 0) {
        throw new Error("The provider returned no usable models.");
      }
      replaceModels(result.models, wantedModel);
      saveConversation();
    } catch (cause) {
      if (destroyed || generation !== requestGeneration) return;
      setError(oneLine(cause));
      retry.hidden = false;
    } finally {
      if (!destroyed && generation === requestGeneration) {
        loadingModels = false;
        render();
      }
    }
  }

  async function submit(): Promise<void> {
    const selectedProvider = provider(providerSelect.value);
    const document = currentDocument;
    const message = input.value.trim();
    if (
      bridge === null || document === null || selectedProvider === null ||
      modelSelect.value === "" || !notice.checked || pendingText !== null ||
      suggesting !== null || !message
    ) return;
    if (messages.length >= MAX_PERSISTED_MESSAGES) {
      setError("This conversation is full. Start a new chat.");
      return;
    }
    if (
      message.includes("\0") ||
      new TextEncoder().encode(message).byteLength > MAX_PERSISTED_USER_MESSAGE_BYTES
    ) {
      setError("Messages must be valid text no larger than 16 KiB.");
      return;
    }

    const model = modelSelect.value;
    const generation = ++requestGeneration;
    const previous = messages.map(({ role, text }) => ({ role, text }));
    pendingText = message;
    input.value = "";
    setError(null);
    render();
    try {
      const chatResponse = await bridge.send({
        document_title: document.title,
        document: document.snapshot(),
        provider: selectedProvider,
        model,
        messages: previous,
        message,
        focus_text: focusText,
        disclosure_version: 1,
      });
      if (destroyed || generation !== requestGeneration || currentDocument?.id !== document.id) {
        return;
      }
      if (
        !chatResponse.text.trim() || chatResponse.text.includes("\0") ||
        new TextEncoder().encode(chatResponse.text).byteLength >
          MAX_PERSISTED_ASSISTANT_MESSAGE_BYTES
      ) throw new Error("The provider returned a response that is too large to use safely.");
      const reportedModel = chatResponse.reported_model ?? chatResponse.requested_model;
      const suggestionRequestId = createRequestId();
      if (!validSuggestionRequestId(suggestionRequestId)) {
        throw new Error("Proof of Thought could not create a safe suggestion retry identifier.");
      }
      messages.push(
        { role: "user", text: message },
        {
          role: "assistant",
          text: chatResponse.text,
          meta: `${PROVIDER_NAMES[chatResponse.provider]} · ${reportedModel}`,
          incomplete: !chatResponse.complete,
          response: chatResponse,
          suggestionRequestId,
        },
      );
      focusText = null;
      saveConversation();
    } catch (cause) {
      if (!destroyed && generation === requestGeneration) {
        input.value = message;
        setError(oneLine(cause));
      }
    } finally {
      if (!destroyed && generation === requestGeneration) {
        pendingText = null;
        render();
      }
    }
  }

  function captureSelection(): void {
    const selected = (currentDocument?.selectedText() ?? "").trim();
    if (!selected) {
      setError("Select some document text first.");
      return;
    }
    if (new TextEncoder().encode(selected).byteLength > MAX_FOCUS_BYTES) {
      setError("The selected text is too large to use as chat focus.");
      return;
    }
    focusText = selected;
    setError(null);
    renderControls();
  }

  async function suggestMessage(message: LocalMessage): Promise<void> {
    const document = currentDocument;
    const chatResponse = message.response;
    if (
      document === null || chatResponse === undefined || !chatResponse.complete ||
      options.suggestResponse === undefined || message.suggested || suggesting !== null ||
      pendingText !== null || message.suggestionRequestId === undefined
    ) return;
    const generation = ++requestGeneration;
    suggesting = message;
    setError(null);
    render();
    try {
      if (!(await document.waitUntilSaved())) {
        throw new Error("Wait for this document to finish saving, then try again.");
      }
      if (destroyed || generation !== requestGeneration || currentDocument?.id !== document.id) {
        return;
      }
      saveConversation();
      await options.suggestResponse({
        documentId: document.id,
        requestId: message.suggestionRequestId,
        provider: chatResponse.provider,
        requestedModel: chatResponse.requested_model,
        reportedModel: chatResponse.reported_model,
        assistantText: chatResponse.text,
        wordingRevision: chatResponse.wording_revision,
        after: document.suggestionPosition(),
      });
      if (destroyed || generation !== requestGeneration || currentDocument?.id !== document.id) {
        return;
      }
      message.suggested = true;
      saveConversation();
      options.onNotice?.("Suggestion added for review.");
    } catch (cause) {
      if (!destroyed && generation === requestGeneration) {
        setError(`Could not create suggestion: ${oneLine(cause)}`);
        options.onNotice?.("Could not create the suggestion.", "error");
      }
    } finally {
      if (!destroyed && generation === requestGeneration) {
        suggesting = null;
        render();
      }
    }
  }

  listen(providerSelect, "change", () => {
    requestGeneration += 1;
    preferredModel = "";
    replaceModels([]);
    focusText = null;
    notice.checked = false;
    if (provider(providerSelect.value) === null) saveConversation();
    render();
    void loadModels();
  });
  listen(modelSelect, "change", () => {
    preferredModel = modelSelect.value;
    saveConversation();
    renderControls();
  });
  listen(retry, "click", () => void loadModels());
  listen(notice, "change", renderControls);
  listen(input, "input", renderControls);
  listen(captureFocus, "click", captureSelection);
  listen(removeFocus, "click", () => {
    focusText = null;
    renderControls();
  });
  listen(form, "submit", (event) => {
    event.preventDefault();
    void submit();
  });
  listen(newChat, "click", newConversation);

  render();
  return {
    setActive(next) {
      const activated = !active && next;
      active = next;
      renderControls();
      if (
        activated && provider(providerSelect.value) !== null &&
        modelSelect.options.length === 0 && !loadingModels
      ) void loadModels();
    },
    setDocument(document) {
      const changed = currentDocument?.id !== document?.id;
      currentDocument = document;
      if (!changed) {
        renderControls();
        return;
      }
      clearTransientState();
      messages = [];
      providerSelect.value = "";
      preferredModel = "";
      replaceModels([]);
      if (document) {
        const saved = readConversation(document.id);
        if (saved) {
          providerSelect.value = saved.provider ?? "";
          preferredModel = saved.model;
          messages = saved.messages;
        }
      }
      render();
      if (active && provider(providerSelect.value) !== null) void loadModels();
    },
    destroy() {
      destroyed = true;
      requestGeneration += 1;
      disposers.splice(0).forEach((dispose) => dispose());
    },
  };
}
