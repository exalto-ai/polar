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
  availability: "guide" | "planned";
  setup: string;
  caveat: string | null;
};

export const AI_CLIENTS: readonly AiClientDefinition[] = [
  {
    id: "chatgpt",
    name: "ChatGPT desktop",
    shortName: "ChatGPT",
    availability: "guide",
    setup:
      "Open Settings, choose MCP servers, then Add server. Choose STDIO, name it Proof of Thought, paste the command below, save, and restart.",
    caveat: "ChatGPT on the web cannot reach this local editor.",
  },
  {
    id: "codex",
    name: "Codex",
    shortName: "Codex",
    availability: "guide",
    setup:
      "Run this once in Terminal. The ChatGPT desktop app and Codex share this MCP configuration on the same host.",
    caveat: "This invokes the Codex CLI already installed on your Mac. Proof of Thought does not install or validate that executable.",
  },
  {
    id: "claude-desktop",
    name: "Claude Desktop",
    shortName: "Claude",
    availability: "planned",
    setup:
      "Claude Desktop uses a local desktop extension. The one-click Proof of Thought extension is tracked in the next connection PR.",
    caveat: "Use Claude Code today if you want to connect through Claude immediately.",
  },
  {
    id: "claude-code",
    name: "Claude Code",
    shortName: "Claude Code",
    availability: "guide",
    setup:
      "Run this once in Terminal to add Proof of Thought for your user, then check the connection with /mcp inside Claude Code.",
    caveat: "This invokes the Claude CLI already installed on your Mac. Proof of Thought does not install or validate that executable.",
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

export function setupCommand(client: AiClient, stdioCommand: string): string | null {
  const command = stdioCommand.trim();
  if (!command || client === "claude-desktop") return null;
  if (client === "codex") return `codex mcp add proof-of-thought -- ${shellArgument(command)}`;
  if (client === "claude-code") {
    return `claude mcp add --scope user proof-of-thought -- ${shellArgument(command)}`;
  }
  return command;
}

function shellArgument(value: string): string {
  if (/^[A-Za-z0-9_./:@%+=,-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

type AiSupportControllerOptions = {
  storage?: Storage | null;
  copyText?: (text: string) => Promise<void>;
  onModeChange?: (mode: AiSupportMode) => void;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type AiSupportController = {
  mode(): AiSupportMode | null;
  isChoosingMode(): boolean;
  isSidebarOpen(): boolean;
  whenInitialChoiceMade(): Promise<AiSupportMode>;
  setConnectionCommand(command: string): void;
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
  const copyText = options.copyText ?? ((text: string) => navigator.clipboard.writeText(text));
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
  let connectionCommand: string | null = null;
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

    const command = startupFailure !== null || connectionCommand === null
      ? null
      : setupCommand(currentClient, connectionCommand);
    const placeholder = selected.availability === "planned"
      ? "Available in the next connection PR"
      : startupFailure
        ? "Local setup is unavailable until Proof of Thought starts"
        : connectionCommand === null
          ? "Loading local setup…"
          : "Local setup command unavailable";
    commandBox.textContent = command ?? placeholder;
    commandBox.dataset.placeholder = String(!command);
    copyButton.disabled = !command;
    copyButton.textContent = command
      ? "Copy setup"
      : selected.availability === "planned"
        ? "Coming next"
        : startupFailure
          ? "Unavailable"
          : "Loading…";
  }

  function renderMode() {
    const connected = currentMode === "connect";
    for (const button of modeButtons) {
      button.setAttribute("aria-pressed", String(button.dataset.aiMode === currentMode));
    }
    toggle.dataset.mode = currentMode ?? "unconfigured";
    toggle.textContent = currentMode === null ? "AI support" : connected ? "AI setup" : "Basic";
    summaryTitle.textContent = connected ? "Reviewer setup" : "Basic recording";
    summaryDescription.textContent = connected
      ? "Choose an AI app to set up. Proof of Thought will record AI tool edits when they arrive, without treating setup as proof of a live connection."
      : "Proof of Thought records how the visible document changed without setting up or calling an AI service.";
    summaryEvidence.textContent = connected
      ? "Setup choice, not connection proof"
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
  listen(copyButton, "click", () => {
    if (connectionCommand === null || startupFailure !== null) return;
    const command = setupCommand(currentClient, connectionCommand);
    if (!command) return;
    void copyText(command)
      .then(() => {
        copyButton.textContent = "Copied";
        window.setTimeout(() => renderClient(), 1200);
      })
      .catch(() => options.onNotice?.("Could not copy the setup command.", "error"));
  });
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
    setConnectionCommand(command: string) {
      connectionCommand = command;
      startupFailure = null;
      renderClient();
      renderSurfaces();
    },
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
