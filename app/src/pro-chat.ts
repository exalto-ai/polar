import type {
  ChatMessage,
  ProChatBridge,
  ProviderModel,
} from "./pro-chat-bridge";
import type { ProProvider } from "./pro-provider-bridge";

const PROVIDER_NAMES: Record<ProProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};

export type ProChatDocument = {
  id: string;
  title: string;
  snapshot(): unknown;
};

type Options = {
  bridge?: ProChatBridge | null;
};

type LocalMessage = ChatMessage & {
  meta?: string;
  incomplete?: boolean;
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

function provider(value: string): ProProvider | null {
  return value === "openai" || value === "anthropic" ? value : null;
}

export function installProChat(
  root: Document,
  options: Options = {},
): ProChatController {
  const panel = required<HTMLElement>(root, "#pro-chat");
  const providerSelect = required<HTMLSelectElement>(panel, "#pro-chat-provider");
  const modelSelect = required<HTMLSelectElement>(panel, "#pro-chat-model");
  const retry = required<HTMLButtonElement>(panel, "#pro-chat-retry");
  const notice = required<HTMLInputElement>(panel, "#pro-chat-consent");
  const documentLabel = required<HTMLElement>(panel, "#pro-chat-document");
  const messagesElement = required<HTMLOListElement>(panel, "#pro-chat-messages");
  const empty = required<HTMLElement>(panel, "#pro-chat-empty");
  const error = required<HTMLElement>(panel, "#pro-chat-error");
  const form = required<HTMLFormElement>(panel, "#pro-chat-form");
  const input = required<HTMLTextAreaElement>(panel, "#pro-chat-input");
  const send = required<HTMLButtonElement>(panel, "#pro-chat-send");
  const newChat = required<HTMLButtonElement>(panel, "#pro-chat-new");
  const bridge = options.bridge ?? null;
  const disposers: Array<() => void> = [];
  let currentDocument: ProChatDocument | null = null;
  let messages: LocalMessage[] = [];
  let pendingText: string | null = null;
  let active = false;
  let loadingModels = false;
  let destroyed = false;
  let requestGeneration = 0;

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
  }

  function renderControls(): void {
    const selectedProvider = provider(providerSelect.value);
    const hasModel = modelSelect.value !== "";
    documentLabel.textContent = currentDocument
      ? `Current document: ${currentDocument.title}`
      : "Open a document to start a chat.";
    modelSelect.disabled = selectedProvider === null || loadingModels;
    retry.disabled = loadingModels || selectedProvider === null || bridge === null;
    input.disabled = currentDocument === null || pendingText !== null || bridge === null;
    send.disabled = currentDocument === null || selectedProvider === null || !hasModel ||
      !notice.checked || pendingText !== null || input.value.trim() === "" ||
      bridge === null;
    panel.setAttribute("aria-busy", String(loadingModels || pendingText !== null));
    send.textContent = pendingText === null ? "Send" : "Sending…";
  }

  function clearConversation(): void {
    requestGeneration += 1;
    messages = [];
    pendingText = null;
    input.value = "";
    setError(null);
    renderMessages();
    renderControls();
  }

  function replaceModels(models: ProviderModel[]): void {
    const options = models.map((model) => {
      const option = root.createElement("option");
      option.value = model.id;
      option.textContent = model.display_name;
      return option;
    });
    modelSelect.replaceChildren(...options);
    modelSelect.value = models[0]?.id ?? "";
  }

  async function loadModels(): Promise<void> {
    const selectedProvider = provider(providerSelect.value);
    const generation = ++requestGeneration;
    replaceModels([]);
    retry.hidden = true;
    setError(null);
    if (selectedProvider === null || bridge === null || !active) {
      renderControls();
      return;
    }
    loadingModels = true;
    renderControls();
    try {
      const result = await bridge.models(selectedProvider);
      if (destroyed || generation !== requestGeneration) return;
      if (result.provider !== selectedProvider || result.models.length === 0) {
        throw new Error("The provider returned no usable models.");
      }
      replaceModels(result.models);
    } catch (cause) {
      if (destroyed || generation !== requestGeneration) return;
      setError(oneLine(cause));
      retry.hidden = false;
    } finally {
      if (!destroyed && generation === requestGeneration) {
        loadingModels = false;
        renderControls();
      }
    }
  }

  async function submit(): Promise<void> {
    const selectedProvider = provider(providerSelect.value);
    const document = currentDocument;
    const message = input.value.trim();
    if (
      bridge === null || document === null || selectedProvider === null ||
      modelSelect.value === "" || !notice.checked || pendingText !== null || !message
    ) return;

    const model = modelSelect.value;
    const generation = ++requestGeneration;
    const previous = messages.map(({ role, text }) => ({ role, text }));
    pendingText = message;
    input.value = "";
    setError(null);
    renderMessages();
    renderControls();
    try {
      const response = await bridge.send({
        document_title: document.title,
        document: document.snapshot(),
        provider: selectedProvider,
        model,
        messages: previous,
        message,
        disclosure_version: 1,
      });
      if (destroyed || generation !== requestGeneration || currentDocument?.id !== document.id) {
        return;
      }
      const reportedModel = response.reported_model ?? response.requested_model;
      messages.push(
        { role: "user", text: message },
        {
          role: "assistant",
          text: response.text,
          meta: `${PROVIDER_NAMES[response.provider]} · ${reportedModel}`,
          incomplete: !response.complete,
        },
      );
    } catch (cause) {
      if (!destroyed && generation === requestGeneration) setError(oneLine(cause));
    } finally {
      if (!destroyed && generation === requestGeneration) {
        pendingText = null;
        renderMessages();
        renderControls();
      }
    }
  }

  listen(providerSelect, "change", () => {
    clearConversation();
    notice.checked = false;
    void loadModels();
  });
  listen(retry, "click", () => void loadModels());
  listen(notice, "change", renderControls);
  listen(input, "input", renderControls);
  listen(form, "submit", (event) => {
    event.preventDefault();
    void submit();
  });
  listen(newChat, "click", clearConversation);

  renderMessages();
  renderControls();
  return {
    setActive(next) {
      active = next;
      renderControls();
    },
    setDocument(document) {
      const changed = currentDocument?.id !== document?.id;
      currentDocument = document;
      if (changed) clearConversation();
      else renderControls();
    },
    destroy() {
      destroyed = true;
      requestGeneration += 1;
      disposers.splice(0).forEach((dispose) => dispose());
    },
  };
}
