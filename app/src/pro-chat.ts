import type {
  ChatAttachment,
  ChatMessage,
  ProChatBridge,
  ProviderModel,
  SendChatResponse,
  ThinkingLevel,
} from "./pro-chat-bridge";
import type { ChatSuggestionInput } from "./editor-api";
import type { ProProvider } from "./pro-provider-bridge";
import type { SuggestionPosition } from "./suggestions";

const PROVIDER_NAMES: Record<ProProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};
const MAX_FOCUS_BYTES = 32 * 1024;
const MAX_ATTACHMENTS = 5;
const MAX_PDF_BYTES = 10 * 1024 * 1024;
const MAX_TEXT_BYTES = 512 * 1024;
const MAX_TOTAL_ATTACHMENT_BYTES = 20 * 1024 * 1024;
const MAX_PERSISTED_MESSAGES = 30;
const MAX_PERSISTED_RECORD_BYTES = 640 * 1024;
const MAX_PERSISTED_USER_MESSAGE_BYTES = 16 * 1024;
const MAX_PERSISTED_ASSISTANT_MESSAGE_BYTES = 64 * 1024;
const MAX_PERSISTED_IDENTIFIER_BYTES = 512;
const MAX_SUGGESTION_METADATA_BYTES = 160;
const MAX_SUGGESTION_REQUEST_ID_BYTES = 128;
const STORAGE_PREFIX = "thought.pro-chat.v1.";
const STORAGE_VERSION = 1;
const TEXT_FILE_NAME = /\.(?:csv|html?|json|log|markdown|md|toml|txt|xml|ya?ml)$/i;
const TEXT_MEDIA_TYPES = new Set([
  "application/json",
  "application/toml",
  "application/xml",
  "application/yaml",
]);

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
  thinking?: ThinkingLevel;
  attachments?: AttachmentSummary[];
  suggestionRequestId?: string;
  suggested?: boolean;
};

type PersistedResponse = Omit<SendChatResponse, "text">;

type PersistedMessage = ChatMessage & {
  response?: PersistedResponse;
  thinking?: ThinkingLevel;
  attachments?: AttachmentSummary[];
  suggestionRequestId?: string;
  suggested?: true;
};

type PersistedConversation = {
  version: typeof STORAGE_VERSION;
  provider: ProProvider | null;
  model: string;
  thinking: ThinkingLevel;
  messages: PersistedMessage[];
};

type RestoredConversation = Omit<PersistedConversation, "messages"> & {
  messages: LocalMessage[];
};

type StagedAttachment = ChatAttachment & { sizeBytes: number };
type AttachmentSummary = {
  name: string;
  media_type: ChatAttachment["media_type"];
  size_bytes: number;
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

function thinkingLevel(value: unknown): ThinkingLevel | null {
  return value === "provider_default" || value === "low" || value === "medium" ||
      value === "high"
    ? value
    : null;
}

function thinking(value: unknown): ThinkingLevel {
  return thinkingLevel(value) ?? "provider_default";
}

function thinkingLabel(value: ThinkingLevel): string {
  switch (value) {
    case "provider_default":
      return "Provider default";
    case "low":
      return "Low";
    case "medium":
      return "Medium";
    case "high":
      return "High";
  }
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

function attachmentSummary(value: unknown): AttachmentSummary | null {
  if (!isRecord(value) || typeof value.name !== "string") return null;
  let name: string;
  try {
    name = attachmentName(value.name);
  } catch {
    return null;
  }
  if (
    (value.media_type !== "application/pdf" && value.media_type !== "text/plain") ||
    typeof value.size_bytes !== "number" || !Number.isSafeInteger(value.size_bytes) ||
    value.size_bytes <= 0
  ) return null;
  const maximum = value.media_type === "application/pdf" ? MAX_PDF_BYTES : MAX_TEXT_BYTES;
  if (value.size_bytes > maximum) return null;
  return { name, media_type: value.media_type, size_bytes: value.size_bytes };
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
  if (value.attachments !== undefined && !Array.isArray(value.attachments)) return null;
  const attachments = Array.isArray(value.attachments)
    ? value.attachments.map(attachmentSummary)
    : [];
  if (
    attachments.length > MAX_ATTACHMENTS || attachments.some((attachment) => attachment === null)
  ) return null;
  const attachmentValues = attachments as AttachmentSummary[];
  if (
    new Set(attachmentValues.map(({ name }) => name)).size !== attachmentValues.length ||
    attachmentValues.reduce((sum, attachment) => sum + attachment.size_bytes, 0) >
      MAX_TOTAL_ATTACHMENT_BYTES ||
    (value.role === "assistant" && savedResponse === undefined) ||
    (savedResponse !== undefined && value.role !== "assistant") ||
    (attachmentValues.length > 0 && value.role !== "user")
  ) return null;
  const requestedThinking = savedResponse ? thinkingLevel(value.thinking) : null;
  if (savedResponse && requestedThinking === null) return null;
  if (savedResponse && suggestionRequestId === undefined) return null;
  const hasAssistantOnlyState = value.response !== undefined || value.thinking !== undefined ||
    value.suggestionRequestId !== undefined || value.suggested !== undefined;
  if (
    (value.role !== "assistant" && hasAssistantOnlyState) ||
    (value.role === "assistant" && attachmentValues.length > 0) ||
    (value.role === "assistant" && hasAssistantOnlyState && savedResponse === undefined) ||
    (value.suggested === true && suggestionRequestId === undefined)
  ) return null;
  const reportedModel = savedResponse?.reported_model ?? savedResponse?.requested_model;
  const thinkingCopy = requestedThinking
    ? `${thinkingLabel(requestedThinking)} thinking requested`
    : null;
  return {
    role: value.role,
    text: value.text,
    response: savedResponse,
    thinking: requestedThinking ?? undefined,
    attachments: attachmentValues.length > 0 ? attachmentValues : undefined,
    suggestionRequestId,
    suggested: value.suggested === true,
    meta: savedResponse
      ? [PROVIDER_NAMES[savedResponse.provider], reportedModel, thinkingCopy]
        .filter(Boolean).join(" · ")
      : undefined,
    incomplete: savedResponse ? !savedResponse.complete : undefined,
  };
}

function persistedConversation(value: unknown): RestoredConversation | null {
  if (
    !isRecord(value) || value.version !== STORAGE_VERSION ||
    !Array.isArray(value.messages) || value.messages.length > MAX_PERSISTED_MESSAGES ||
    !withinBytes(value.model, MAX_PERSISTED_IDENTIFIER_BYTES) || value.model.includes("\0") ||
    thinkingLevel(value.thinking) === null
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
    thinking: value.thinking as ThinkingLevel,
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
    saved.thinking = message.thinking ?? "provider_default";
  }
  if (message.attachments?.length) {
    saved.attachments = message.attachments.map((attachment) => ({ ...attachment }));
  }
  if (message.suggestionRequestId) saved.suggestionRequestId = message.suggestionRequestId;
  if (message.suggested) saved.suggested = true;
  return saved;
}

function storageKey(documentId: string): string {
  return `${STORAGE_PREFIX}${encodeURIComponent(documentId)}`;
}

function attachmentMediaType(
  file: File,
  name: string,
): ChatAttachment["media_type"] | null {
  const mediaType = file.type.toLowerCase();
  if (mediaType === "application/pdf" || name.toLowerCase().endsWith(".pdf")) {
    return "application/pdf";
  }
  if (
    mediaType.startsWith("text/") || TEXT_MEDIA_TYPES.has(mediaType) ||
    TEXT_FILE_NAME.test(name)
  ) return "text/plain";
  return null;
}

function attachmentName(value: string): string {
  const trimmed = value.trim();
  if (
    !trimmed || trimmed === "." || trimmed === ".." ||
    /[\\/\u0000-\u001f\u007f]/.test(trimmed) ||
    new TextEncoder().encode(trimmed).byteLength > 200
  ) {
    throw new Error(
      "Attachment names must be 200 UTF-8 bytes or fewer and cannot contain paths or control characters.",
    );
  }
  return trimmed;
}

function displayFileName(value: string): string {
  return value.replace(/[\r\n\t]+/g, " ").trim().slice(0, 80) || "This file";
}

function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function base64(bytes: Uint8Array): string {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 32_768) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 32_768));
  }
  return btoa(binary);
}

function isPdf(bytes: Uint8Array): boolean {
  return bytes.length >= 5 && bytes[0] === 0x25 && bytes[1] === 0x50 &&
    bytes[2] === 0x44 && bytes[3] === 0x46 && bytes[4] === 0x2d;
}

export function installProChat(
  root: Document,
  options: Options = {},
): ProChatController {
  const panel = required<HTMLElement>(root, "#pro-chat");
  const providerSelect = required<HTMLSelectElement>(panel, "#pro-chat-provider");
  const modelSelect = required<HTMLSelectElement>(panel, "#pro-chat-model");
  const thinkingSelect = required<HTMLSelectElement>(panel, "#pro-chat-thinking");
  const retry = required<HTMLButtonElement>(panel, "#pro-chat-retry");
  const storageNotice = required<HTMLElement>(panel, "#pro-chat-storage-notice");
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
  const attach = required<HTMLButtonElement>(panel, "#pro-chat-attach");
  const attachmentInput = required<HTMLInputElement>(panel, "#pro-chat-attachment-input");
  const attachmentList = required<HTMLUListElement>(panel, "#pro-chat-attachments");
  const bridge = options.bridge ?? null;
  const createRequestId = options.createRequestId ?? (() => crypto.randomUUID());
  const disposers: Array<() => void> = [];
  let currentDocument: ProChatDocument | null = null;
  let messages: LocalMessage[] = [];
  let stagedAttachments: StagedAttachment[] = [];
  let pendingText: string | null = null;
  let suggesting: LocalMessage | null = null;
  let focusText: string | null = null;
  let preferredModel = "";
  let active = false;
  let loadingModels = false;
  let readingAttachments = false;
  let destroyed = false;
  let requestGeneration = 0;
  let attachmentGeneration = 0;
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
      thinking: thinking(thinkingSelect.value),
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
    if (message.attachments?.length) {
      const attachments = root.createElement("small");
      attachments.className = "pro-chat-message-attachments";
      attachments.textContent = `Attached: ${message.attachments.map((attachment) =>
        `${attachment.name} (${fileSize(attachment.size_bytes)})`).join(", ")}`;
      item.append(attachments);
    }
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
        pendingText !== null || readingAttachments;
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
    newChat.disabled = loadingModels || pendingText !== null || suggesting !== null ||
      readingAttachments;
  }

  function removeAttachment(index: number): void {
    stagedAttachments.splice(index, 1);
    attachmentInput.value = "";
    setError(null);
    render();
  }

  function renderAttachments(): void {
    const busy = pendingText !== null || suggesting !== null || readingAttachments;
    const items = stagedAttachments.map((attachment, index) => {
      const item = root.createElement("li");
      const name = root.createElement("span");
      name.textContent = `${attachment.name} · ${fileSize(attachment.sizeBytes)}`;
      name.title = attachment.name;
      const remove = root.createElement("button");
      remove.type = "button";
      remove.className = "text-button";
      remove.textContent = "Remove";
      remove.setAttribute("aria-label", `Remove ${attachment.name}`);
      remove.disabled = busy;
      remove.addEventListener("click", () => removeAttachment(index));
      item.append(name, remove);
      return item;
    });
    attachmentList.replaceChildren(...items);
    attachmentList.hidden = items.length === 0;
  }

  function renderControls(): void {
    const selectedProvider = provider(providerSelect.value);
    const hasModel = modelSelect.value !== "";
    const busy = pendingText !== null || suggesting !== null || readingAttachments;
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
    attach.disabled = currentDocument === null || selectedProvider === null || busy ||
      stagedAttachments.length >= MAX_ATTACHMENTS;
    attachmentInput.disabled = attach.disabled;
    modelSelect.disabled = selectedProvider === null || loadingModels || busy;
    thinkingSelect.disabled = selectedProvider === null || !hasModel || loadingModels || busy;
    retry.disabled = loadingModels || selectedProvider === null || bridge === null || busy;
    input.disabled = currentDocument === null || busy || bridge === null;
    send.disabled = currentDocument === null || selectedProvider === null || !hasModel ||
      busy || input.value.trim() === "" || bridge === null;
    panel.setAttribute("aria-busy", String(loadingModels || busy));
    send.textContent = pendingText === null ? "Send" : "Sending…";
  }

  function render(): void {
    renderMessages();
    renderAttachments();
    renderControls();
  }

  function clearAttachments(): void {
    attachmentGeneration += 1;
    stagedAttachments = [];
    readingAttachments = false;
    attachmentInput.value = "";
  }

  function clearTransientState(): void {
    requestGeneration += 1;
    loadingModels = false;
    retry.hidden = true;
    pendingText = null;
    suggesting = null;
    focusText = null;
    input.value = "";
    clearAttachments();
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

  async function stageFiles(files: readonly File[]): Promise<void> {
    const documentId = currentDocument?.id;
    const generation = ++attachmentGeneration;
    attachmentInput.value = "";
    if (documentId === undefined || files.length === 0) return;
    if (stagedAttachments.length + files.length > MAX_ATTACHMENTS) {
      setError("Attach no more than 5 files to one request.");
      render();
      return;
    }
    readingAttachments = true;
    setError(null);
    render();
    try {
      const next: StagedAttachment[] = [];
      let totalBytes = stagedAttachments.reduce((sum, file) => sum + file.sizeBytes, 0);
      const names = new Set(stagedAttachments.map(({ name }) => name));
      for (const file of files) {
        const name = attachmentName(file.name);
        if (names.has(name)) {
          throw new Error(`“${displayFileName(name)}” is already attached.`);
        }
        const mediaType = attachmentMediaType(file, name);
        if (mediaType === null) {
          throw new Error("Only PDF and UTF-8 text files can be attached.");
        }
        const maximum = mediaType === "application/pdf" ? MAX_PDF_BYTES : MAX_TEXT_BYTES;
        const limit = mediaType === "application/pdf" ? "10 MiB" : "512 KiB";
        if (file.size <= 0) {
          throw new Error(`“${displayFileName(name)}” is empty.`);
        }
        if (file.size > maximum) {
          throw new Error(`“${displayFileName(name)}” exceeds the ${limit} limit.`);
        }
        if (totalBytes + file.size > MAX_TOTAL_ATTACHMENT_BYTES) {
          throw new Error("Attachments cannot exceed 20 MiB in total.");
        }
        const bytes = new Uint8Array(await file.arrayBuffer());
        if (
          destroyed || generation !== attachmentGeneration ||
          currentDocument?.id !== documentId
        ) return;
        if (bytes.byteLength > maximum) {
          throw new Error(`“${displayFileName(name)}” exceeds the ${limit} limit.`);
        }
        if (bytes.byteLength === 0) {
          throw new Error(`“${displayFileName(name)}” is empty.`);
        }
        if (totalBytes + bytes.byteLength > MAX_TOTAL_ATTACHMENT_BYTES) {
          throw new Error("Attachments cannot exceed 20 MiB in total.");
        }
        if (mediaType === "application/pdf" && !isPdf(bytes)) {
          throw new Error(`“${displayFileName(name)}” is not a valid PDF file.`);
        }
        if (mediaType === "text/plain") {
          try {
            const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
            if (text.includes("\0")) throw new Error("null byte");
          } catch {
            throw new Error(`“${displayFileName(name)}” is not a UTF-8 text file.`);
          }
        }
        next.push({
          name,
          media_type: mediaType,
          content_base64: base64(bytes),
          sizeBytes: bytes.byteLength,
        });
        names.add(name);
        totalBytes += bytes.byteLength;
      }
      stagedAttachments.push(...next);
    } catch (cause) {
      if (
        !destroyed && generation === attachmentGeneration &&
        currentDocument?.id === documentId
      ) setError(oneLine(cause));
    } finally {
      if (
        !destroyed && generation === attachmentGeneration &&
        currentDocument?.id === documentId
      ) {
        readingAttachments = false;
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
      modelSelect.value === "" || pendingText !== null ||
      suggesting !== null || readingAttachments || !message
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
    const selectedThinking = thinking(thinkingSelect.value);
    const generation = ++requestGeneration;
    const previous = messages.map(({ role, text }) => ({ role, text }));
    const requestAttachments: ChatAttachment[] = stagedAttachments.map((attachment) => ({
      name: attachment.name,
      media_type: attachment.media_type,
      content_base64: attachment.content_base64,
    }));
    const attachmentSummaries: AttachmentSummary[] = stagedAttachments.map((attachment) => ({
      name: attachment.name,
      media_type: attachment.media_type,
      size_bytes: attachment.sizeBytes,
    }));
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
        thinking: selectedThinking,
        messages: previous,
        message,
        focus_text: focusText,
        attachments: requestAttachments,
        disclosure_version: 2,
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
      const thinkingCopy = `${thinkingLabel(selectedThinking)} thinking requested`;
      const suggestionRequestId = createRequestId();
      if (!validSuggestionRequestId(suggestionRequestId)) {
        throw new Error("Proof of Thought could not create a safe suggestion retry identifier.");
      }
      messages.push(
        {
          role: "user",
          text: message,
          attachments: attachmentSummaries.length > 0 ? attachmentSummaries : undefined,
        },
        {
          role: "assistant",
          text: chatResponse.text,
          meta: `${PROVIDER_NAMES[chatResponse.provider]} · ${reportedModel} · ${thinkingCopy}`,
          incomplete: !chatResponse.complete,
          response: chatResponse,
          thinking: selectedThinking,
          suggestionRequestId,
        },
      );
      focusText = null;
      clearAttachments();
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
      pendingText !== null || readingAttachments || message.suggestionRequestId === undefined
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
    clearAttachments();
    if (provider(providerSelect.value) === null) saveConversation();
    render();
    void loadModels();
  });
  listen(modelSelect, "change", () => {
    preferredModel = modelSelect.value;
    saveConversation();
    renderControls();
  });
  listen(thinkingSelect, "change", () => {
    thinkingSelect.value = thinking(thinkingSelect.value);
    saveConversation();
    renderControls();
  });
  listen(retry, "click", () => void loadModels());
  listen(input, "input", renderControls);
  listen(captureFocus, "click", captureSelection);
  listen(removeFocus, "click", () => {
    focusText = null;
    renderControls();
  });
  listen(attach, "click", () => attachmentInput.click());
  listen(attachmentInput, "change", () => {
    void stageFiles(Array.from(attachmentInput.files ?? []));
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
      thinkingSelect.value = "provider_default";
      preferredModel = "";
      replaceModels([]);
      if (document) {
        const saved = readConversation(document.id);
        if (saved) {
          providerSelect.value = saved.provider ?? "";
          thinkingSelect.value = saved.thinking;
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
      clearAttachments();
      disposers.splice(0).forEach((dispose) => dispose());
    },
  };
}
