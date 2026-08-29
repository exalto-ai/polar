export const AI_SUPPORT_STORAGE_KEY = "thought.ai-support.v1";

export type AiSupportMode = "basic" | "connect";
export type AiClient = "chatgpt" | "codex" | "claude-desktop" | "claude-code";

type StoredPreference = {
  version: 1;
  mode: AiSupportMode;
};

export type AiClientDefinition = {
  id: AiClient;
  name: string;
  shortName: string;
  availability: "planned";
  setup: string;
  caveat: string | null;
};

export const AI_CLIENTS: readonly AiClientDefinition[] = [
  {
    id: "chatgpt",
    name: "ChatGPT desktop",
    shortName: "ChatGPT",
    availability: "planned",
    setup:
      "A guided local reviewer connection for ChatGPT desktop is coming in the next update.",
    caveat: "ChatGPT on the web cannot reach this local editor.",
  },
  {
    id: "codex",
    name: "Codex",
    shortName: "Codex",
    availability: "planned",
    setup:
      "A guided local reviewer connection for Codex is coming in the next update.",
    caveat: "Proof of Thought will not install or validate the Codex executable.",
  },
  {
    id: "claude-desktop",
    name: "Claude Desktop",
    shortName: "Claude",
    availability: "planned",
    setup:
      "A guided local reviewer connection for Claude Desktop is coming in the next update.",
    caveat: "Claude Desktop will require a packaged and tested local extension.",
  },
  {
    id: "claude-code",
    name: "Claude Code",
    shortName: "Claude Code",
    availability: "planned",
    setup:
      "A guided local reviewer connection for Claude Code is coming in the next update.",
    caveat: "Proof of Thought will not install or validate the Claude executable.",
  },
] as const;

export function safeLocalStorage(target: { readonly localStorage: Storage }): Storage | null {
  try {
    return target.localStorage;
  } catch {
    return null;
  }
}

export function readAiSupportMode(storage: Storage | null): AiSupportMode | null {
  if (storage === null) return null;
  try {
    const raw = storage.getItem(AI_SUPPORT_STORAGE_KEY);
    if (!raw) return null;
    const value = JSON.parse(raw) as Partial<StoredPreference>;
    if (value.version !== 1) return null;
    return value.mode === "basic" || value.mode === "connect" ? value.mode : null;
  } catch {
    return null;
  }
}

export function writeAiSupportMode(storage: Storage | null, mode: AiSupportMode): boolean {
  if (storage === null) return false;
  try {
    const value: StoredPreference = { version: 1, mode };
    storage.setItem(AI_SUPPORT_STORAGE_KEY, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

type AiSupportControllerOptions = {
  storage?: Storage | null;
  onModeChange?: (mode: AiSupportMode) => void;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type AiSupportController = {
  mode(): AiSupportMode | null;
  isChoosingMode(): boolean;
  isSidebarOpen(): boolean;
  whenInitialChoiceMade(): Promise<AiSupportMode>;
  setStartupError(message: string): void;
  openSidebar(): void;
  closeSidebar(): void;
  showModePicker(): void;
  dismissModePicker(): boolean;
  destroy(): void;
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing AI support element: ${selector}`);
  return value;
}

export function installAiSupport(
  root: Document,
  options: AiSupportControllerOptions = {},
): AiSupportController {
  const storage = options.storage === undefined ? safeLocalStorage(window) : options.storage;
  const toggle = required<HTMLButtonElement>(root, "#ai-support-toggle");
  const sidebar = required<HTMLElement>(root, "#ai-support-sidebar");
  const sidebarClose = required<HTMLButtonElement>(root, "#ai-sidebar-close");
  const changeMode = required<HTMLButtonElement>(root, "#change-ai-mode");
  const summaryTitle = required<HTMLElement>(root, "#ai-mode-title");
  const summaryDescription = required<HTMLElement>(root, "#ai-mode-description");
  const summaryEvidence = required<HTMLElement>(root, "#ai-mode-evidence");
  const summaryCost = required<HTMLElement>(root, "#ai-mode-cost");
  const connectPanel = required<HTMLElement>(root, "#ai-connect-panel");
  const basicPanel = required<HTMLElement>(root, "#ai-basic-panel");
  const clientSetup = required<HTMLElement>(root, "#ai-client-setup");
  const clientCaveat = required<HTMLElement>(root, "#ai-client-caveat");
  const commandBox = required<HTMLElement>(root, "#ai-connection-command");
  const copyButton = required<HTMLButtonElement>(root, "#copy-ai-command");
  const modal = required<HTMLElement>(root, "#ai-onboarding");
  const modalDialog = required<HTMLElement>(modal, '[role="dialog"]');
  const modalHeading = required<HTMLElement>(modal, "#ai-onboarding-title");
  const modalClose = required<HTMLButtonElement>(root, "#ai-onboarding-close");
  const startupError = required<HTMLElement>(root, "#ai-startup-error");
  const startupErrorMessage = required<HTMLElement>(root, "#ai-startup-error-message");
  const clientButtons = [
    ...root.querySelectorAll<HTMLButtonElement>("[data-ai-client]"),
  ];
  const modeButtons = [
    ...root.querySelectorAll<HTMLButtonElement>("[data-ai-mode]"),
  ];

  let currentMode = readAiSupportMode(storage);
  let currentClient: AiClient = "chatgpt";
  let startupFailure: string | null = null;
  let sidebarOpen = false;
  let modalOpen = currentMode === null;
  let returnFocus: HTMLElement | null = null;
  let resolveInitialChoice: ((mode: AiSupportMode) => void) | null = null;
  const initialChoice = currentMode === null
    ? new Promise<AiSupportMode>((resolve) => {
        resolveInitialChoice = resolve;
      })
    : Promise.resolve(currentMode);
  const disposers: Array<() => void> = [];
  const backgroundBlockedElements = new Set<HTMLElement>();

  function listen<K extends keyof DocumentEventMap>(
    target: Document,
    event: K,
    listener: (event: DocumentEventMap[K]) => void,
  ): void;
  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    event: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ): void;
  function listen<K extends keyof WindowEventMap>(
    target: Window,
    event: K,
    listener: (event: WindowEventMap[K]) => void,
  ): void;
  function listen(
    target: Document | HTMLElement | Window,
    event: string,
    listener: EventListener,
  ) {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function client(): AiClientDefinition {
    return AI_CLIENTS.find(({ id }) => id === currentClient)!;
  }

  function renderClient() {
    const selected = client();
    for (const button of clientButtons) {
      const active = button.dataset.aiClient === currentClient;
      button.setAttribute("aria-pressed", String(active));
      button.dataset.availability = AI_CLIENTS.find(
        ({ id }) => id === button.dataset.aiClient,
      )?.availability;
    }
    clientSetup.textContent = selected.setup;
    clientCaveat.textContent = selected.caveat ?? "";
    clientCaveat.hidden = !selected.caveat;

    commandBox.textContent = "Connection setup will be available in the next update.";
    commandBox.dataset.placeholder = "true";
    copyButton.disabled = true;
    copyButton.textContent = "Available in the next update";
  }

  function renderMode() {
    const connected = currentMode === "connect";
    for (const button of modeButtons) {
      button.setAttribute("aria-pressed", String(button.dataset.aiMode === currentMode));
    }
    toggle.dataset.mode = currentMode ?? "unconfigured";
    toggle.textContent = currentMode === null ? "AI support" : connected ? "AI preview" : "Basic";
    summaryTitle.textContent = connected ? "Reviewer setup" : "Basic recording";
    summaryDescription.textContent = connected
      ? "Choose an AI app to preview how reviewer setup will work. Connection setup is available in the next update."
      : "Proof of Thought records how the visible document changed without setting up or calling an AI service.";
    summaryEvidence.textContent = connected
      ? "Planned setup, not connection proof"
      : "Local edit evidence";
    summaryEvidence.dataset.assurance = connected ? "setup" : "observed";
    summaryCost.textContent = connected
      ? "Proof of Thought adds no API usage charge. Availability and limits depend on your AI app and plan."
      : "Proof of Thought makes no AI request. Existing external tool connections are managed in that AI app.";
    connectPanel.hidden = !connected;
    basicPanel.hidden = connected;
    if (currentMode !== null) options.onModeChange?.(currentMode);
  }

  function setBackgroundBlocked(blocked: boolean) {
    if (blocked) {
      for (const element of root.body.children) {
        if (element === modal || !(element instanceof HTMLElement)) continue;
        if (!element.hasAttribute("inert")) {
          element.setAttribute("inert", "");
          backgroundBlockedElements.add(element);
        }
      }
      return;
    }
    for (const element of backgroundBlockedElements) element.removeAttribute("inert");
    backgroundBlockedElements.clear();
  }

  function renderSurfaces() {
    sidebar.hidden = !sidebarOpen;
    toggle.setAttribute("aria-expanded", String(sidebarOpen));
    modal.hidden = !modalOpen;
    modalClose.hidden = currentMode === null || startupFailure !== null;
    startupError.hidden = startupFailure === null;
    startupErrorMessage.textContent = startupFailure ?? "";
    setBackgroundBlocked(modalOpen);
    root.documentElement.dataset.aiSupportMode = currentMode ?? "unconfigured";
    if (modalOpen) {
      queueMicrotask(() => {
        modalDialog.scrollTop = 0;
        modalHeading.focus({ preventScroll: true });
      });
    }
  }

  function closeModal() {
    if (currentMode === null || startupFailure !== null) return;
    modalOpen = false;
    renderSurfaces();
    returnFocus?.focus();
    returnFocus = null;
  }

  function chooseMode(mode: AiSupportMode) {
    currentMode = mode;
    if (!writeAiSupportMode(storage, mode)) {
      options.onNotice?.(
        "Your choice works for this window, but could not be saved for the next launch.",
        "error",
      );
    }
    modalOpen = startupFailure !== null;
    sidebarOpen = !modalOpen && mode === "connect";
    resolveInitialChoice?.(mode);
    resolveInitialChoice = null;
    renderMode();
    renderSurfaces();
    if (!modalOpen) {
      queueMicrotask(() => (mode === "connect" ? sidebarClose : toggle).focus());
    }
  }

  function openSidebar() {
    if (modalOpen) return;
    sidebarOpen = true;
    renderSurfaces();
    queueMicrotask(() => sidebarClose.focus());
  }

  function closeSidebar() {
    sidebarOpen = false;
    renderSurfaces();
    toggle.focus();
  }

  function showModePicker() {
    returnFocus = root.activeElement instanceof HTMLElement ? root.activeElement : null;
    modalOpen = true;
    renderSurfaces();
  }

  function dismissModePicker(): boolean {
    if (!modalOpen || currentMode === null || startupFailure !== null) return false;
    closeModal();
    return true;
  }

  listen(toggle, "click", () => (sidebarOpen ? closeSidebar() : openSidebar()));
  listen(sidebarClose, "click", closeSidebar);
  listen(changeMode, "click", showModePicker);
  listen(modalClose, "click", closeModal);
  listen(modal, "mousedown", (event) => {
    if (event.target === modal) closeModal();
  });
  for (const button of modeButtons) {
    const value = button.dataset.aiMode;
    if (value !== "basic" && value !== "connect") continue;
    listen(button, "click", () => chooseMode(value));
    listen(button, "keydown", (event) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      chooseMode(value);
    });
  }
  for (const button of clientButtons) {
    listen(button, "click", () => {
      const value = button.dataset.aiClient as AiClient;
      if (!AI_CLIENTS.some(({ id }) => id === value)) return;
      currentClient = value;
      renderClient();
    });
  }
  listen(window, "storage", (event) => {
    if (event.key !== AI_SUPPORT_STORAGE_KEY) return;
    const mode = readAiSupportMode(storage);
    if (mode === null) return;
    currentMode = mode;
    modalOpen = startupFailure !== null;
    sidebarOpen = false;
    resolveInitialChoice?.(mode);
    resolveInitialChoice = null;
    renderMode();
    renderSurfaces();
    if (!modalOpen) queueMicrotask(() => toggle.focus());
  });
  listen(root, "keydown", (event) => {
    if (event.key === "Escape") {
      if (modalOpen) {
        event.preventDefault();
        event.stopImmediatePropagation();
        if (currentMode !== null && startupFailure === null) closeModal();
      } else if (!modalOpen && sidebarOpen) {
        event.preventDefault();
        event.stopImmediatePropagation();
        closeSidebar();
      }
      return;
    }
    if (!modalOpen || event.key !== "Tab") return;
    const controls = [
      ...modal.querySelectorAll<HTMLElement>(
        "button:not([disabled]):not([hidden]), summary, a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
      ),
    ];
    if (controls.length === 0) return;
    const current = controls.indexOf(root.activeElement as HTMLElement);
    const next = event.shiftKey
      ? (current <= 0 ? controls.length : current) - 1
      : (current + 1) % controls.length;
    event.preventDefault();
    controls[next].focus();
  });

  renderMode();
  renderClient();
  renderSurfaces();

  return {
    mode: () => currentMode,
    isChoosingMode: () => modalOpen,
    isSidebarOpen: () => sidebarOpen,
    whenInitialChoiceMade: () => initialChoice,
    setStartupError(message: string) {
      startupFailure = message.trim() || "Unknown startup problem.";
      modalOpen = true;
      sidebarOpen = false;
      renderClient();
      renderSurfaces();
    },
    openSidebar,
    closeSidebar,
    showModePicker,
    dismissModePicker,
    destroy() {
      for (const dispose of disposers.splice(0)) dispose();
      setBackgroundBlocked(false);
      delete root.documentElement.dataset.aiSupportMode;
    },
  };
}
