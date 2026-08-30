import {
  PRO_CHAT_DISCLOSURE_VERSION,
  type ProChatBridge,
  type ProChatEvent,
  type ProChatHistory,
  type ProChatModel,
  type ProChatProvider,
  type ProChatProviderCapability,
  type ProChatSuggestionPosition,
  type ProChatThinking,
  type ProChatTurn,
} from "./pro-chat-bridge";
import { writeClipboardText } from "./clipboard";

export type ProChatDocumentContext = {
  id: string;
  title: string;
  snapshot: () => unknown;
  suggestionPosition: () => ProChatSuggestionPosition;
  waitUntilSaved: () => Promise<boolean>;
};

type ProChatOptions = {
  bridge?: ProChatBridge | null;
  onOpenSettings?: () => void;
  onInitialAvailabilityResolved?: (hasUsableProvider: boolean) => void;
  onBusyChange?: (provider: ProChatProvider | null) => void;
  onActivityChange?: (active: boolean) => void;
  copyText?: (text: string) => Promise<void>;
  onResponseCopied?: () => void;
  onSuggestionCreated?: (suggestionId: string) => void;
  createRequestId?: () => string;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type ProChatController = {
  setActive(active: boolean): void;
  setDocument(context: ProChatDocumentContext | null): void;
  refreshCapabilities(): Promise<void>;
  cancelActive(): void;
  destroy(): void;
};

type RunningRequest = {
  documentId: string;
  provider: ProChatProvider;
  operationId: string | null;
  turnId: string | null;
  draft: string | null;
  settled: boolean;
  cancelRequested: boolean;
};

type MessageNodes = {
  item: HTMLLIElement;
  assistant: HTMLElement;
  assistantMeta: HTMLElement;
  status: HTMLElement;
  retry: HTMLButtonElement;
  actions: HTMLElement;
  copy: HTMLButtonElement;
  suggest: HTMLButtonElement;
};

const THINKING_LEVELS: readonly ProChatThinking[] = [
  "default",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

const MAX_MESSAGE_BYTES = 16 * 1024;
const utf8 = new TextEncoder();

const THINKING_LABEL: Record<ProChatThinking, string> = {
  default: "Provider default",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Maximum",
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing Pro chat element: ${selector}`);
  return value;
}

function shortError(cause: unknown): string {
  const raw = cause instanceof Error ? cause.message : String(cause);
  const clean = raw.replace(/[\r\n\t]+/g, " ").trim();
  return clean
    ? clean.slice(0, 180)
    : "The provider request could not be completed.";
}

function isConversationChangedError(cause: unknown): boolean {
  const message = cause instanceof Error ? cause.message : String(cause);
  return message.toLowerCase().includes("conversation changed");
}

function turnFailureCopy(turn: ProChatTurn, eventMessage?: string | null): string {
  if (eventMessage?.trim()) return eventMessage.trim().slice(0, 240);
  if (turn.status === "stopped") {
    return "The response was stopped. API charges may still apply for work already done.";
  }
  switch (turn.error_category) {
    case "authentication":
      return "The provider key needs attention in Settings.";
    case "permission":
      return "The provider key does not have permission for this request.";
    case "billing":
    case "spend_or_usage_limit":
      return "The provider reported a billing, credit, or usage-limit problem.";
    case "rate_limited":
      return "The provider is limiting requests right now. Try again shortly.";
    case "provider_unavailable":
      return "The provider is temporarily unavailable.";
    case "timeout":
      return "The provider did not respond in time.";
    case "network_or_tls_failure":
      return "Proof of Thought could not securely reach the provider.";
    case "model_unavailable":
      return "That model is not available for this provider account.";
    case "conversation_changed":
      return "The local conversation changed before this request could start. Reload it and try again.";
    case "storage":
      return "Proof of Thought could not save this local conversation.";
    case "invalid_request":
      return "The provider could not use these model or thinking settings.";
    case "invalid_provider_response":
      return "The provider returned a response Proof of Thought could not read safely.";
    case "refusal":
      return "The provider declined this request. This response will not be included in later chat context.";
    default:
      return "The response could not be completed.";
  }
}

function messageIssueCopy(value: string): string | null {
  if (value.includes("\0")) {
    return "This message contains unsupported text. Remove it before sending.";
  }
  if (utf8.encode(value).byteLength > MAX_MESSAGE_BYTES) {
    return "This message is too long to send. Shorten it to 16 KiB or less.";
  }
  return null;
}

function statusCopy(turn: ProChatTurn): string {
  switch (turn.status) {
    case "pending":
      return "Responding…";
    case "completed":
      return "Complete";
    case "stopped":
      return "Stopped";
    case "interrupted":
      return "Interrupted";
    case "incomplete":
      return "Incomplete";
    case "failed":
      return "Could not complete";
  }
}

function option(value: string, label: string): HTMLOptionElement {
  const element = document.createElement("option");
  element.value = value;
  element.textContent = label;
  return element;
}

export function installProChat(
  root: Document,
  options: ProChatOptions = {},
): ProChatController {
  const panel = required<HTMLElement>(root, "#pro-chat-view");
  const documentLabel = required<HTMLElement>(panel, "#pro-chat-document");
  const unavailable = required<HTMLElement>(panel, "#pro-chat-unavailable");
  const openSettings = required<HTMLButtonElement>(panel, "#pro-chat-open-settings");
  const controls = required<HTMLDetailsElement>(panel, "#pro-chat-controls");
  const controlsSummary = required<HTMLElement>(controls, "summary");
  const selectionSummary = required<HTMLElement>(controls, "#pro-chat-selection-summary");
  const modelSelect = required<HTMLSelectElement>(panel, "#pro-chat-model");
  const thinkingSelect = required<HTMLSelectElement>(panel, "#pro-chat-thinking");
  const loading = required<HTMLElement>(panel, "#pro-chat-loading");
  const empty = required<HTMLElement>(panel, "#pro-chat-empty");
  const messages = required<HTMLOListElement>(panel, "#pro-chat-messages");
  const clearButton = required<HTMLButtonElement>(panel, "#pro-chat-clear");
  const clearConfirmation = required<HTMLElement>(panel, "#pro-chat-clear-confirmation");
  const clearCancel = required<HTMLButtonElement>(panel, "#pro-chat-clear-cancel");
  const clearConfirm = required<HTMLButtonElement>(panel, "#pro-chat-clear-confirm");
  const error = required<HTMLElement>(panel, "#pro-chat-error");
  const errorMessage = required<HTMLElement>(panel, "#pro-chat-error-message");
  const errorRetry = required<HTMLButtonElement>(panel, "#pro-chat-error-retry");
  const composer = required<HTMLFormElement>(panel, "#pro-chat-composer");
  const message = required<HTMLTextAreaElement>(panel, "#pro-chat-message");
  const messageIssue = required<HTMLElement>(panel, "#pro-chat-message-issue");
  const sharing = required<HTMLElement>(panel, "#pro-chat-sharing");
  const consent = required<HTMLInputElement>(panel, "#pro-chat-consent");
  const consentLabel = required<HTMLElement>(panel, ".pro-chat-consent");
  const consentCopy = required<HTMLElement>(panel, "#pro-chat-consent-copy");
  const billingReminder = required<HTMLElement>(panel, "#pro-chat-billing-reminder");
  const send = required<HTMLButtonElement>(panel, "#pro-chat-send");
  const stop = required<HTMLButtonElement>(panel, "#pro-chat-stop");
  const live = required<HTMLElement>(panel, "#pro-chat-live");
  const bridge = options.bridge ?? null;
  const copyText = options.copyText ?? writeClipboardText;
  const createRequestId = options.createRequestId ?? (() => crypto.randomUUID());
  const disposers: Array<() => void> = [];
  const consentedProviders = new Set<ProChatProvider>();
  const messageNodes = new Map<string, MessageNodes>();
  const turnErrors = new Map<string, string>();

  let active = false;
  let destroyed = false;
  let documentContext: ProChatDocumentContext | null = null;
  let pendingDocumentContext: ProChatDocumentContext | null = null;
  let documentChangePending = false;
  let providers: ProChatProviderCapability[] = [];
  let selectedProvider: ProChatProvider | null = null;
  let selectedModel = "";
  let selectedThinking: ProChatThinking = "default";
  let history: ProChatHistory | null = null;
  let capabilitiesLoading = false;
  let historyLoading = false;
  let clearing = false;
  let stopping = false;
  let clearOpen = false;
  let clearReturnFocus: HTMLButtonElement | null = null;
  let loadError: string | null = null;
  let retryLoad: (() => void) | null = null;
  let capabilityGeneration = 0;
  let historyGeneration = 0;
  let clearGeneration = 0;
  let running: RunningRequest | null = null;
  let suggestingTurnId: string | null = null;
  const suggestedTurnIds = new Set<string>();
  let initialAvailabilityResolved = false;

  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    event: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ): void {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function provider(): ProChatProviderCapability | null {
    return providers.find(({ provider }) => provider === selectedProvider) ?? null;
  }

  function model(): ProChatModel | null {
    return provider()?.models.find(({ id }) => id === selectedModel) ?? null;
  }

  function providerName(value = selectedProvider): string {
    if (value === null) return "your provider";
    return providers.find(({ provider }) => provider === value)?.display_name ??
      (value === "openai" ? "OpenAI" : "Anthropic");
  }

  function isReady(): boolean {
    return Boolean(
      bridge &&
        documentContext &&
        selectedProvider &&
        selectedModel &&
        model(),
    );
  }

  function isBusy(): boolean {
    return running !== null || clearing || suggestingTurnId !== null;
  }

  function publishBusy(): void {
    options.onBusyChange?.(isBusy() ? selectedProvider : null);
    options.onActivityChange?.(running !== null);
  }

  function visibleMessageCount(): number {
    return (history?.turns ?? []).reduce(
      (count, turn) => count + (turn.status === "completed" ? 2 : 0),
      0,
    );
  }

  function renderSharing(): void {
    const name = providerName();
    const earlier = visibleMessageCount();
    sharing.textContent = earlier === 0
      ? `To ${name}: this document’s title and contents, including formatting and links, plus this message. No files are attached. API charges apply.`
      : `To ${name}: this document’s title and contents, including formatting and links, this message, and ${earlier} completed earlier chat messages. No files are attached. API charges apply.`;
    consentCopy.textContent = `I agree to send this document’s title and contents, including formatting and links, and visible chat to ${name}. API charges may apply.`;
  }

  function responseText(turnId: string): string {
    return history?.turns.find(({ id }) => id === turnId)?.assistant_text ?? "";
  }

  function makeMessageNodes(turn: ProChatTurn): MessageNodes {
    const item = document.createElement("li");
    item.className = "pro-chat-turn";
    item.dataset.turnId = turn.id;

    const user = document.createElement("article");
    user.className = "pro-chat-message user";
    user.setAttribute("aria-label", "You");
    const userLabel = document.createElement("span");
    userLabel.className = "pro-chat-message-label";
    userLabel.textContent = "You";
    const userText = document.createElement("p");
    userText.textContent = turn.user_text;
    user.append(userLabel, userText);

    const assistantArticle = document.createElement("article");
    assistantArticle.className = "pro-chat-message assistant";
    const assistantHeader = document.createElement("header");
    const assistantLabel = document.createElement("span");
    assistantLabel.className = "pro-chat-message-label";
    assistantLabel.textContent = providerName(turn.provider);
    const assistantMeta = document.createElement("span");
    assistantMeta.className = "pro-chat-message-meta";
    assistantHeader.append(assistantLabel, assistantMeta);
    const assistant = document.createElement("p");
    const footer = document.createElement("footer");
    const status = document.createElement("span");
    status.className = "pro-chat-turn-status";
    const retry = document.createElement("button");
    retry.type = "button";
    retry.className = "text-button pro-chat-turn-retry";
    retry.textContent = "Try again";
    retry.addEventListener("click", () => void retryTurn(turn.id));
    const actions = document.createElement("span");
    actions.className = "pro-chat-reply-actions";
    actions.setAttribute("role", "group");
    actions.setAttribute("aria-label", "Use this response");
    const copy = document.createElement("button");
    copy.type = "button";
    copy.textContent = "Copy";
    copy.addEventListener("click", () => {
      const text = responseText(turn.id);
      if (!text) return;
      copy.disabled = true;
      void Promise.resolve()
        .then(() => copyText(text))
        .then(() => {
          if (!copy.isConnected) return;
          copy.textContent = "Copied";
          options.onNotice?.("Copied. Press Command-V to paste in the editor.");
          try {
            options.onResponseCopied?.();
          } catch {
            // Copy succeeded. A focus handoff failure must not relabel it.
          }
          window.setTimeout(() => {
            if (!copy.isConnected) return;
            copy.textContent = "Copy";
            copy.disabled = false;
          }, 1400);
        })
        .catch(() => {
          if (!copy.isConnected) return;
          copy.disabled = false;
          options.onNotice?.("Could not copy this response.", "error");
        });
    });
    const suggest = document.createElement("button");
    suggest.type = "button";
    suggest.className = "pro-chat-turn-suggest";
    suggest.textContent = "Suggest in document";
    suggest.addEventListener("click", () => void suggestTurn(turn.id));
    actions.append(retry, copy, suggest);
    footer.append(status, actions);
    assistantArticle.append(assistantHeader, assistant, footer);
    item.append(user, assistantArticle);
    return { item, assistant, assistantMeta, status, retry, actions, copy, suggest };
  }

  function renderTurns(): void {
    const atBottom = messages.scrollHeight - messages.scrollTop - messages.clientHeight < 48;
    const turns = history?.turns ?? [];
    const present = new Set(turns.map(({ id }) => id));
    for (const [id, nodes] of messageNodes) {
      if (present.has(id)) continue;
      nodes.item.remove();
      messageNodes.delete(id);
      turnErrors.delete(id);
    }

    for (const turn of turns) {
      let nodes = messageNodes.get(turn.id);
      if (!nodes) {
        nodes = makeMessageNodes(turn);
        messageNodes.set(turn.id, nodes);
      }
      const userText = required<HTMLElement>(nodes.item, ".pro-chat-message.user p");
      const providerLabel = required<HTMLElement>(nodes.item, ".pro-chat-message.assistant .pro-chat-message-label");
      userText.textContent = turn.user_text;
      providerLabel.textContent = providerName(turn.provider);
      nodes.assistantMeta.textContent = turn.reported_model
        ? `Provider reported ${turn.reported_model}`
        : `Requested ${turn.requested_model}`;
      const failure = turnErrors.get(turn.id) ?? turnFailureCopy(turn);
      nodes.assistant.textContent = turn.assistant_text ||
        (turn.status === "pending"
          ? "Responding…"
          : turn.status === "stopped"
            ? "Stopped before a response was returned."
            : turn.status === "failed" || turn.status === "interrupted" || turn.status === "incomplete"
              ? failure
              : "No text was returned.");
      nodes.status.textContent = statusCopy(turn);
      if (
        turn.status !== "pending" &&
        turn.status !== "completed" &&
        (turn.assistant_text.length > 0 || turn.status === "stopped")
      ) {
        nodes.status.textContent = `${statusCopy(turn)}. ${failure}`;
      }
      nodes.status.dataset.status = turn.status;
      nodes.retry.hidden = !turn.retryable || turn.status === "pending";
      nodes.retry.disabled = isBusy() || !consentedProviders.has(turn.provider);
      const responseReady = turn.status !== "pending" && turn.assistant_text.length > 0;
      nodes.actions.hidden = !responseReady && nodes.retry.hidden;
      nodes.copy.disabled = !responseReady || nodes.copy.textContent === "Copied";
      const canSuggest = turn.status === "completed" &&
        turn.assistant_text.length > 0 && turn.wording_revision.length > 0;
      nodes.suggest.hidden = !canSuggest;
      nodes.suggest.disabled = !canSuggest || isBusy() || suggestedTurnIds.has(turn.id);
      nodes.suggest.textContent = suggestingTurnId === turn.id
        ? "Suggesting…"
        : suggestedTurnIds.has(turn.id)
          ? "Suggested"
          : "Suggest in document";
      messages.append(nodes.item);
    }

    if (atBottom) messages.scrollTop = messages.scrollHeight;
    empty.hidden = historyLoading || !isReady() || turns.length !== 0;
    messages.hidden = !isReady() || turns.length === 0;
  }

  function render(): void {
    const ready = isReady();
    const busy = isBusy();
    const hasProviders = providers.some(({ models }) => models.length > 0);
    panel.setAttribute("aria-busy", String(capabilitiesLoading || historyLoading || busy));
    documentLabel.textContent = documentContext
      ? `Conversation for “${documentContext.title.trim() || "Untitled"}”`
      : "Waiting for this document to open…";
    unavailable.hidden = capabilitiesLoading || ready;
    if (!unavailable.hidden) {
      const heading = required<HTMLElement>(unavailable, "strong");
      const detail = required<HTMLElement>(unavailable, "span");
      if (bridge === null) {
        heading.textContent = "Built-in chat is unavailable";
        detail.textContent = "This build cannot reach the native provider service.";
      } else if (!documentContext) {
        heading.textContent = "Waiting for this document";
        detail.textContent = "Chat will be available after the document finishes opening.";
      } else if (!hasProviders) {
        heading.textContent = "Add a provider to start chatting";
        detail.textContent = "OpenAI or Anthropic can be set up without sending document content.";
      } else {
        heading.textContent = "No compatible chat model is available";
        detail.textContent = "Check the provider again in Settings.";
      }
    }
    openSettings.hidden = bridge === null;
    controls.hidden = !ready;
    composer.hidden = !ready;
    modelSelect.disabled = busy || historyLoading;
    thinkingSelect.disabled = busy || historyLoading;
    controlsSummary.setAttribute("aria-disabled", String(busy || historyLoading));
    if ((busy || historyLoading) && controls.open) controls.open = false;
    loading.hidden = !capabilitiesLoading && !historyLoading;
    loading.textContent = capabilitiesLoading
      ? "Checking available providers and models…"
      : "Loading this conversation…";
    clearButton.disabled = busy || historyLoading || (history?.turns.length ?? 0) === 0;
    clearConfirmation.hidden = !clearOpen;
    clearConfirm.disabled = clearing;
    clearCancel.disabled = clearing;
    error.hidden = loadError === null;
    errorMessage.textContent = loadError ?? "";
    errorRetry.hidden = retryLoad === null;
    errorRetry.disabled = busy;
    const providerConsent = selectedProvider !== null && consentedProviders.has(selectedProvider);
    const messageIssueText = messageIssueCopy(message.value);
    consent.checked = providerConsent;
    consent.disabled = busy || !ready;
    sharing.hidden = providerConsent;
    consentLabel.hidden = providerConsent;
    billingReminder.hidden = !providerConsent;
    message.disabled = busy || historyLoading || !ready;
    message.setAttribute(
      "aria-describedby",
      providerConsent
        ? "pro-chat-billing-reminder pro-chat-message-issue"
        : "pro-chat-sharing pro-chat-consent-copy pro-chat-message-issue",
    );
    messageIssue.hidden = messageIssueText === null;
    messageIssue.textContent = messageIssueText ?? "";
    message.setAttribute("aria-invalid", String(messageIssueText !== null));
    send.disabled = busy || historyLoading || !ready || !providerConsent ||
      !message.value.trim() || messageIssueText !== null;
    send.hidden = running !== null;
    stop.hidden = running === null;
    stop.disabled = stopping || running?.operationId === null;
    stop.textContent = stopping ? "Stopping…" : "Stop";
    renderSharing();
    const selected = model();
    selectionSummary.textContent = selected
      ? `${selected.display_name || selected.id} · ${THINKING_LABEL[selectedThinking]}`
      : "Choose a model";
    renderTurns();
  }

  function selectThinkingForModel(preferred = selectedThinking): void {
    const supported = new Set(model()?.thinking_levels ?? []);
    thinkingSelect.replaceChildren(
      ...THINKING_LEVELS.map((level) => {
        const item = option(level, THINKING_LABEL[level]);
        item.disabled = !supported.has(level);
        return item;
      }),
    );
    selectedThinking = supported.has(preferred)
      ? preferred
      : supported.has("default")
        ? "default"
        : model()?.thinking_levels[0] ?? "default";
    thinkingSelect.value = selectedThinking;
  }

  function modelChoiceValue(provider: ProChatProvider, modelId: string): string {
    return JSON.stringify([provider, modelId]);
  }

  function readModelChoice(value: string): [ProChatProvider, string] | null {
    try {
      const parsed = JSON.parse(value) as unknown;
      if (
        Array.isArray(parsed) &&
        parsed.length === 2 &&
        (parsed[0] === "openai" || parsed[0] === "anthropic") &&
        typeof parsed[1] === "string"
      ) return [parsed[0], parsed[1]];
    } catch {
      // A stale or injected option is ignored below.
    }
    return null;
  }

  function populateModels(
    preferredProvider = selectedProvider,
    preferredModel = selectedModel,
  ): void {
    const usable = providers.filter(({ models }) => models.length > 0);
    modelSelect.replaceChildren(
      ...usable.map(({ provider, display_name, models }) => {
        const group = document.createElement("optgroup");
        group.label = display_name;
        group.append(...models.map(({ id, display_name: modelName }) =>
          option(modelChoiceValue(provider, id), modelName || id)));
        return group;
      }),
    );
    const preferred = usable.find(({ provider }) => provider === preferredProvider);
    const selectedCapability = preferred ?? usable[0] ?? null;
    selectedProvider = selectedCapability?.provider ?? null;
    selectedModel = selectedCapability?.models.some(({ id }) => id === preferredModel)
      ? preferredModel
      : selectedCapability?.models[0]?.id ?? "";
    modelSelect.value = selectedProvider
      ? modelChoiceValue(selectedProvider, selectedModel)
      : "";
    selectThinkingForModel();
  }

  function applyHistory(value: ProChatHistory): void {
    history = {
      ...value,
      turns: value.turns.map((turn) => ({ ...turn })),
    };
    turnErrors.clear();
    render();
  }

  function applyDocumentContext(context: ProChatDocumentContext | null): void {
    documentContext = context;
    historyGeneration += 1;
    clearGeneration += 1;
    history = null;
    turnErrors.clear();
    message.value = "";
    live.textContent = "";
    clearOpen = false;
    loadError = null;
    retryLoad = null;
    clearing = false;
    stopping = false;
    suggestingTurnId = null;
    suggestedTurnIds.clear();
    publishBusy();
    if (active && selectedProvider) void loadHistory();
    else render();
  }

  function applyPendingDocumentChange(): boolean {
    if (!documentChangePending) return false;
    const context = pendingDocumentContext;
    documentChangePending = false;
    pendingDocumentContext = null;
    applyDocumentContext(context);
    return true;
  }

  async function loadHistory(preserveClearIntent = false): Promise<boolean> {
    const context = documentContext;
    const targetProvider = selectedProvider;
    const generation = ++historyGeneration;
    const retainedClearIntent = preserveClearIntent && clearOpen;
    history = null;
    turnErrors.clear();
    loadError = null;
    retryLoad = null;
    clearOpen = retainedClearIntent;
    if (!active || bridge === null || context === null || targetProvider === null) {
      historyLoading = false;
      render();
      return false;
    }
    historyLoading = true;
    render();
    try {
      const value = await bridge.history(context.id, targetProvider);
      if (
        destroyed ||
        generation !== historyGeneration ||
        documentContext?.id !== context.id ||
        selectedProvider !== targetProvider
      ) return false;
      clearOpen = retainedClearIntent;
      applyHistory(value);
      return true;
    } catch (cause) {
      if (destroyed || generation !== historyGeneration) return false;
      loadError = `Could not load this conversation: ${shortError(cause)}`;
      retryLoad = () => void loadHistory(preserveClearIntent);
      return false;
    } finally {
      if (!destroyed && generation === historyGeneration) {
        historyLoading = false;
        render();
      }
    }
  }

  async function refreshCapabilities(): Promise<void> {
    if (bridge === null || destroyed) {
      providers = [];
      render();
      return;
    }
    const generation = ++capabilityGeneration;
    const previousProvider = selectedProvider;
    const previousModel = selectedModel;
    capabilitiesLoading = true;
    loadError = null;
    retryLoad = null;
    render();
    try {
      const value = await bridge.capabilities();
      if (destroyed || generation !== capabilityGeneration) return;
      providers = value.providers.filter(
        ({ provider }) => provider === "openai" || provider === "anthropic",
      );
      populateModels(previousProvider, previousModel);
      if (!initialAvailabilityResolved) {
        initialAvailabilityResolved = true;
        options.onInitialAvailabilityResolved?.(
          providers.some(({ models }) => models.length > 0),
        );
      }
      await loadHistory();
    } catch (cause) {
      if (destroyed || generation !== capabilityGeneration) return;
      providers = [];
      selectedProvider = null;
      selectedModel = "";
      history = null;
      loadError = `Could not check available providers: ${shortError(cause)}`;
      retryLoad = () => void refreshCapabilities();
    } finally {
      if (!destroyed && generation === capabilityGeneration) {
        capabilitiesLoading = false;
        render();
      }
    }
  }

  function upsertTurn(turn: ProChatTurn): void {
    if (history === null || history.provider !== turn.provider) return;
    const index = history.turns.findIndex(({ id }) => id === turn.id);
    if (index === -1) history.turns.push({ ...turn });
    else history.turns[index] = { ...turn };
  }

  function handleEvent(context: RunningRequest, event: ProChatEvent): void {
    if (destroyed || running !== context) return;
    if (context.operationId !== null && event.operation_id !== context.operationId) return;
    context.operationId ??= event.operation_id;
    const visible = documentContext?.id === context.documentId &&
      selectedProvider === context.provider;

    if (event.type === "started") {
      context.turnId = event.turn.id;
      if (context.draft !== null && message.value === context.draft) {
        message.value = "";
      }
      if (visible && history) {
        history.revision = event.revision;
        upsertTurn(event.turn);
        live.textContent = `${providerName(context.provider)} is responding.`;
      }
      render();
      if (context.cancelRequested) void stopRunning();
      return;
    }

    if (event.type === "delta") {
      if (visible && history) {
        const turn = history.turns.find(({ id }) => id === event.turn_id);
        if (turn) turn.assistant_text += event.text;
      }
      render();
      return;
    }

    if (visible && history) {
      history.revision = event.revision;
      upsertTurn(event.turn);
      if (event.error_message) {
        turnErrors.set(event.turn.id, turnFailureCopy(event.turn, event.error_message));
      }
      live.textContent = event.type === "completed"
        ? `${providerName(context.provider)} finished responding.`
        : event.type === "stopped"
          ? "The response stopped."
          : "The response could not be completed.";
    }
    running = null;
    context.settled = true;
    stopping = false;
    const restoreChatFocus = visible && panel.contains(root.activeElement);
    publishBusy();
    if (applyPendingDocumentChange()) return;
    render();
    if (restoreChatFocus) {
      const retry = messageNodes.get(event.turn.id)?.retry;
      if (event.type !== "completed" && retry && !retry.hidden) retry.focus();
      else message.focus();
    }
  }

  async function startTurn(
    visibleMessage: string | null,
    retryTurn: ProChatTurn | null,
  ): Promise<void> {
    const context = documentContext;
    const targetProvider = retryTurn?.provider ?? selectedProvider;
    const targetModel = retryTurn?.requested_model ?? selectedModel;
    const targetThinking = retryTurn?.thinking ?? selectedThinking;
    if (
      bridge === null ||
      context === null ||
      targetProvider === null ||
      history === null ||
      running !== null ||
      clearing ||
      !consentedProviders.has(targetProvider) ||
      (visibleMessage === null && retryTurn === null) ||
      (visibleMessage !== null &&
        (!visibleMessage.trim() || messageIssueCopy(visibleMessage) !== null))
    ) return;

    let documentSnapshot: unknown;
    try {
      documentSnapshot = context.snapshot();
    } catch (cause) {
      loadError = `Could not read the current document: ${shortError(cause)}`;
      retryLoad = null;
      render();
      return;
    }

    const requestContext: RunningRequest = {
      documentId: context.id,
      provider: targetProvider,
      operationId: null,
      turnId: retryTurn?.id ?? null,
      draft: visibleMessage,
      settled: false,
      cancelRequested: false,
    };
    running = requestContext;
    loadError = null;
    retryLoad = null;
    clearOpen = false;
    publishBusy();
    render();
    const draft = visibleMessage;
    try {
      const result = await bridge.start(
        {
          document_id: context.id,
          document_title: context.title,
          document: documentSnapshot,
          provider: targetProvider,
          expected_revision: history.revision,
          model: targetModel,
          thinking: targetThinking,
          message: visibleMessage,
          retry_turn_id: retryTurn?.id ?? null,
          disclosure_version: PRO_CHAT_DISCLOSURE_VERSION,
        },
        (event) => handleEvent(requestContext, event),
      );
      if (destroyed || running !== requestContext) {
        if (!requestContext.settled) {
          void bridge.stop(result.operation_id).catch(() => undefined);
        }
        return;
      }
      if (
        requestContext.operationId !== null &&
        requestContext.operationId !== result.operation_id
      ) {
        running = null;
        loadError = "The provider response could not be matched to this request.";
        retryLoad = null;
        publishBusy();
        void bridge.stop(result.operation_id).catch(() => undefined);
        if (!applyPendingDocumentChange()) render();
        return;
      }
      requestContext.operationId = result.operation_id;
      requestContext.turnId ??= result.turn_id;
      if (draft !== null && message.value === draft) message.value = "";
      render();
      if (requestContext.cancelRequested) void stopRunning();
    } catch (cause) {
      if (destroyed || running !== requestContext) return;
      const restoreChatFocus = panel.contains(root.activeElement);
      running = null;
      requestContext.settled = true;
      stopping = false;
      if (applyPendingDocumentChange()) return;
      if (draft !== null && documentContext?.id === context.id) message.value = draft;
      if (isConversationChangedError(cause)) {
        publishBusy();
        render();
        const refreshed = await loadHistory();
        if (
          destroyed ||
          documentContext?.id !== context.id ||
          selectedProvider !== targetProvider
        ) return;
        if (refreshed) {
          loadError = retryTurn
            ? "Conversation updated. Latest messages are loaded. Choose Try again on the response when you are ready."
            : "Conversation updated. Latest messages are loaded. Review your draft, then choose Send again.";
          retryLoad = null;
          render();
        }
        if (restoreChatFocus && panel.contains(root.activeElement)) message.focus();
        return;
      }
      loadError = `Could not start the response: ${shortError(cause)}`;
      retryLoad = retryTurn
        ? () => void retryTurnByValue(retryTurn)
        : () => void startTurn(draft, null);
      publishBusy();
      render();
      if (restoreChatFocus) message.focus();
    }
  }

  async function retryTurnByValue(turn: ProChatTurn): Promise<void> {
    if (!turn.retryable || turn.status === "pending") return;
    await startTurn(null, turn);
  }

  async function retryTurn(turnId: string): Promise<void> {
    const turn = history?.turns.find(({ id }) => id === turnId);
    if (turn) await retryTurnByValue(turn);
  }

  async function suggestTurn(turnId: string): Promise<void> {
    const context = documentContext;
    const turn = history?.turns.find(({ id }) => id === turnId);
    if (
      bridge === null || context === null || turn?.status !== "completed" ||
      !turn.assistant_text || !turn.wording_revision || isBusy()
    ) return;
    suggestingTurnId = turnId;
    publishBusy();
    render();
    try {
      if (!(await context.waitUntilSaved())) {
        throw new Error("Wait for this document to finish saving, then try again.");
      }
      if (documentContext !== context || !history?.turns.some(({ id }) => id === turnId)) {
        return;
      }
      const result = await bridge.suggestResponse({
        documentId: context.id,
        provider: turn.provider,
        turnId,
        requestId: createRequestId(),
        after: context.suggestionPosition(),
      });
      if (documentContext !== context) return;
      suggestedTurnIds.add(turnId);
      options.onSuggestionCreated?.(result.suggestion_id);
      options.onNotice?.("Suggestion added for review.");
    } catch (cause) {
      options.onNotice?.(`Could not create suggestion: ${shortError(cause)}`, "error");
    } finally {
      if (suggestingTurnId === turnId) suggestingTurnId = null;
      publishBusy();
      render();
    }
  }

  async function stopRunning(): Promise<void> {
    const request = running;
    if (bridge === null || request === null || request.operationId === null || stopping) return;
    stopping = true;
    render();
    try {
      const accepted = await bridge.stop(request.operationId);
      if (destroyed || running !== request) return;
      if (!accepted) {
        stopping = false;
        loadError = "The response had already finished before it could be stopped.";
        retryLoad = null;
        render();
      }
    } catch (cause) {
      if (destroyed || running !== request) return;
      stopping = false;
      loadError = `Could not stop the response: ${shortError(cause)}`;
      retryLoad = null;
      render();
    }
  }

  async function clearHistory(): Promise<void> {
    const context = documentContext;
    const targetProvider = selectedProvider;
    const currentHistory = history;
    if (
      bridge === null ||
      context === null ||
      targetProvider === null ||
      currentHistory === null ||
      isBusy()
    ) return;
    const generation = ++clearGeneration;
    clearing = true;
    loadError = null;
    retryLoad = null;
    publishBusy();
    render();
    try {
      const value = await bridge.clear(
        context.id,
        targetProvider,
        currentHistory.revision,
      );
      if (
        destroyed ||
        generation !== clearGeneration ||
        documentContext?.id !== context.id ||
        selectedProvider !== targetProvider
      ) return;
      historyGeneration += 1;
      applyHistory(value);
      clearOpen = false;
      live.textContent = "This document’s local chat was cleared. Its document proof and reviewer history were not changed.";
      options.onNotice?.("Conversation cleared");
    } catch (cause) {
      if (destroyed || generation !== clearGeneration) return;
      if (isConversationChangedError(cause)) {
        const refreshed = await loadHistory(true);
        if (
          destroyed ||
          generation !== clearGeneration ||
          documentContext?.id !== context.id ||
          selectedProvider !== targetProvider
        ) return;
        if (refreshed) {
          clearOpen = true;
          loadError = "Conversation updated. Latest messages are loaded. Confirm Clear chat again to remove them.";
          retryLoad = null;
          render();
        }
        return;
      }
      loadError = `Could not clear this conversation: ${shortError(cause)}`;
      retryLoad = () => void clearHistory();
    } finally {
      if (!destroyed && generation === clearGeneration) {
        const restoreChatFocus = panel.contains(root.activeElement);
        clearing = false;
        publishBusy();
        render();
        if (!clearOpen && restoreChatFocus) message.focus();
      }
    }
  }

  listen(openSettings, "click", () => options.onOpenSettings?.());
  listen(controlsSummary, "click", (event) => {
    if (controlsSummary.getAttribute("aria-disabled") !== "true") return;
    event.preventDefault();
  });
  listen(controls, "keydown", (event) => {
    if (event.key !== "Escape" || !controls.open) return;
    event.preventDefault();
    controls.open = false;
    controlsSummary.focus();
  });
  listen(modelSelect, "change", () => {
    const choice = readModelChoice(modelSelect.value);
    if (!choice) {
      populateModels();
      render();
      return;
    }
    const [nextProvider, nextModel] = choice;
    const nextCapability = providers.find(({ provider }) => provider === nextProvider);
    if (!nextCapability?.models.some(({ id }) => id === nextModel)) {
      populateModels();
      render();
      return;
    }
    const providerChanged = selectedProvider !== nextProvider;
    selectedProvider = nextProvider;
    selectedModel = nextModel;
    selectedThinking = "default";
    consent.checked = consentedProviders.has(selectedProvider);
    selectThinkingForModel("default");
    if (providerChanged) void loadHistory();
    else render();
  });
  listen(thinkingSelect, "change", () => {
    selectedThinking = thinkingSelect.value as ProChatThinking;
    render();
  });
  listen(consent, "change", () => {
    if (selectedProvider) {
      if (consent.checked) consentedProviders.add(selectedProvider);
      else consentedProviders.delete(selectedProvider);
    }
    render();
  });
  listen(message, "input", render);
  listen(message, "keydown", (event) => {
    if (
      event.key !== "Enter" ||
      event.shiftKey ||
      event.isComposing ||
      event.keyCode === 229
    ) return;
    event.preventDefault();
    if (!send.disabled) composer.requestSubmit();
  });
  listen(composer, "submit", (event) => {
    event.preventDefault();
    if (send.disabled) return;
    void startTurn(message.value, null);
  });
  listen(stop, "click", () => void stopRunning());
  listen(clearButton, "click", () => {
    if (clearButton.disabled) return;
    clearReturnFocus = clearButton;
    clearOpen = true;
    render();
    queueMicrotask(() => clearCancel.focus());
  });
  listen(clearCancel, "click", () => {
    clearOpen = false;
    render();
    const returnFocus = clearReturnFocus;
    clearReturnFocus = null;
    returnFocus?.focus();
  });
  listen(clearConfirmation, "keydown", (event) => {
    if (event.key !== "Escape" || !clearOpen || clearing) return;
    event.preventDefault();
    clearOpen = false;
    render();
    const returnFocus = clearReturnFocus;
    clearReturnFocus = null;
    returnFocus?.focus();
  });
  listen(clearConfirm, "click", () => void clearHistory());
  listen(errorRetry, "click", () => retryLoad?.());

  render();

  return {
    setActive(nextActive) {
      const becameActive = nextActive && !active;
      active = nextActive;
      if (becameActive) void refreshCapabilities();
      else render();
    },
    setDocument(context) {
      if (documentContext?.id === context?.id) {
        documentChangePending = false;
        pendingDocumentContext = null;
        documentContext = context;
        render();
        return;
      }
      if (running !== null) {
        pendingDocumentContext = context;
        documentChangePending = true;
        running.cancelRequested = true;
        if (running.operationId !== null) void stopRunning();
        render();
        return;
      }
      applyDocumentContext(context);
    },
    refreshCapabilities,
    cancelActive() {
      if (running === null) return;
      running.cancelRequested = true;
      if (running.operationId !== null) void stopRunning();
    },
    destroy() {
      destroyed = true;
      active = false;
      capabilityGeneration += 1;
      historyGeneration += 1;
      documentChangePending = false;
      pendingDocumentContext = null;
      if (running?.operationId) void bridge?.stop(running.operationId).catch(() => undefined);
      running = null;
      clearing = false;
      publishBusy();
      for (const dispose of disposers.splice(0)) dispose();
      messages.replaceChildren();
      messageNodes.clear();
    },
  };
}
