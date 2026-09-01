import { writeClipboardText } from "./clipboard";
import {
  installDirectEditAccess,
  type DirectEditApi,
} from "./direct-edit-access";
import type { ChatSuggestionInput } from "./editor-api";
import type { ProProviderBridge } from "./pro-provider-bridge";
import type { ProChatBridge } from "./pro-chat-bridge";
import { installProChat, type ProChatDocument } from "./pro-chat";
import { installProProvider } from "./pro-provider";
import {
  installReviewerConnections,
  type ReviewerApi,
} from "./reviewer-connections";

export const AI_SUPPORT_PATH_STORAGE_KEY = "thought.ai-support-path.v1";

export type AiSupportPath = "connected" | "builtin" | "basic";

function isAiSupportPath(value: unknown): value is AiSupportPath {
  return value === "connected" || value === "builtin" || value === "basic";
}

type StoredPreference = {
  version: 1;
  path: AiSupportPath;
};

type AiSupportOptions = {
  storage?: Storage | null;
  copyText?: (text: string) => Promise<void>;
  reviewerApi?: ReviewerApi | null;
  directEditApi?: DirectEditApi | null;
  providerBridge?: ProProviderBridge | null;
  chatBridge?: ProChatBridge | null;
  suggestChatResponse?: (input: ChatSuggestionInput) => Promise<unknown>;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type AiSupportController = {
  isOpen(): boolean;
  path(): AiSupportPath | null;
  isChoosingPath(): boolean;
  showOnboardingIfNeeded(): boolean;
  setConnectionCommand(command: string): void;
  setReviewerApi(api: ReviewerApi | null): void;
  setDirectEditApi(api: DirectEditApi | null): void;
  setCurrentDocument(context: ProChatDocument | null): void;
  open(): void;
  close(): void;
  destroy(): void;
};

const pathDescriptions: Record<AiSupportPath, string> = {
  connected:
    "Use an AI app you already have. Its proposals appear in the document for you to accept or reject.",
  builtin:
    "Chat inside Proof of Thought with your own provider key, then turn useful answers into reviewable suggestions.",
  basic:
    "Use local recording without built-in chat. Existing external reviewers keep their access until you remove them.",
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing AI support element: ${selector}`);
  return value;
}

export function safeLocalStorage(target: {
  readonly localStorage: Storage;
}): Storage | null {
  try {
    return target.localStorage;
  } catch {
    return null;
  }
}

export function readAiSupportPath(storage: Storage | null): AiSupportPath | null {
  if (storage === null) return null;
  try {
    const raw = storage.getItem(AI_SUPPORT_PATH_STORAGE_KEY);
    if (!raw) return null;
    const value = JSON.parse(raw) as Partial<StoredPreference>;
    if (value.version !== 1) return null;
    return isAiSupportPath(value.path) ? value.path : null;
  } catch {
    return null;
  }
}

export function writeAiSupportPath(
  storage: Storage | null,
  path: AiSupportPath,
): boolean {
  if (storage === null) return false;
  try {
    const value: StoredPreference = { version: 1, path };
    storage.setItem(AI_SUPPORT_PATH_STORAGE_KEY, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

/**
 * Own the shared sidebar and first-launch path chooser. The selected path is a
 * presentation preference only. Reviewer credentials and permissions remain
 * owned by the daemon and are never stored here.
 */
export function installAiSupport(
  root: Document,
  options: AiSupportOptions = {},
): AiSupportController {
  const storage = options.storage === undefined
    ? safeLocalStorage(window)
    : options.storage;
  const copyText = options.copyText ?? writeClipboardText;
  const toggle = required<HTMLButtonElement>(root, "#ai-support-toggle");
  const sidebar = required<HTMLElement>(root, "#ai-support-sidebar");
  const closeButton = required<HTMLButtonElement>(root, "#ai-sidebar-close");
  const title = required<HTMLElement>(root, "#ai-support-title");
  const description = required<HTMLElement>(root, "#ai-mode-description");
  const connectedPanel = required<HTMLElement>(root, "#ai-connect-panel");
  const builtinPanel = required<HTMLElement>(root, "#ai-pro-panel");
  const basicPanel = required<HTMLElement>(root, "#ai-basic-panel");
  const onboarding = required<HTMLElement>(root, "#ai-onboarding");
  const onboardingDialog = required<HTMLElement>(onboarding, '[role="dialog"]');
  const onboardingTitle = required<HTMLElement>(onboarding, "#ai-onboarding-title");
  const onboardingClose = required<HTMLButtonElement>(
    onboarding,
    "#ai-onboarding-close",
  );
  const pathButtons = [
    ...root.querySelectorAll<HTMLButtonElement>("[data-ai-mode]"),
  ];
  const disposers: Array<() => void> = [];
  const backgroundBlockedElements = new Set<HTMLElement>();
  const reviewers = installReviewerConnections(root, {
    api: options.reviewerApi,
    copyText,
    onNotice: options.onNotice,
  });
  const chat = root.querySelector("#pro-chat")
    ? installProChat(root, {
        bridge: options.chatBridge,
        suggestResponse: options.suggestChatResponse,
        onNotice: options.onNotice,
      })
    : null;
  const providers = root.querySelector("#provider-settings")
    ? installProProvider(root, {
        bridge: options.providerBridge,
        onNotice: options.onNotice,
      })
    : null;
  let currentPath = readAiSupportPath(storage);
  let sidebarOpen = false;
  let onboardingOpen = false;
  let onboardingReady = false;
  let returnFocus: HTMLElement | null = null;
  const directEdits = installDirectEditAccess(root, {
    api: options.directEditApi,
    canPrompt: () => currentPath !== null && !onboardingOpen,
    onNotice: options.onNotice,
  });

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

  function setBackgroundBlocked(blocked: boolean) {
    if (blocked) {
      for (const element of root.body.children) {
        if (element === onboarding || !(element instanceof HTMLElement)) continue;
        if (!element.hasAttribute("inert")) {
          element.setAttribute("inert", "");
          backgroundBlockedElements.add(element);
        }
      }
      return;
    }
    for (const element of backgroundBlockedElements) {
      element.removeAttribute("inert");
    }
    backgroundBlockedElements.clear();
  }

  function render() {
    const path = currentPath;
    const connected = path === "connected";
    const builtin = path === "builtin";
    connectedPanel.hidden = !connected;
    builtinPanel.hidden = !builtin;
    basicPanel.hidden = path !== "basic";
    description.textContent = path === null
      ? "Choose the level of AI support you want."
      : pathDescriptions[path];
    title.textContent = connected
      ? "Connected app"
      : builtin
        ? "Built-in AI"
        : path === "basic"
          ? "Basic recording"
          : "Choose how to work";
    for (const button of pathButtons) {
      const selected = button.dataset.aiMode === path;
      button.setAttribute("aria-pressed", String(selected));
    }
    toggle.dataset.mode = path ?? "unconfigured";
    toggle.setAttribute("aria-expanded", String(sidebarOpen));
    sidebar.hidden = !sidebarOpen;
    onboarding.hidden = !onboardingOpen;
    onboardingClose.hidden = currentPath === null;
    setBackgroundBlocked(onboardingOpen);
    reviewers.setOpen(sidebarOpen && connected);
    chat?.setActive(sidebarOpen && builtin);
    providers?.setActive(sidebarOpen && builtin);
    root.documentElement.dataset.aiSupportPath = path ?? "unconfigured";
  }

  function focusEditorOrToggle() {
    const editor = root.querySelector<HTMLElement>("#editor .tiptap");
    (editor ?? toggle).focus();
  }

  function closeOnboarding() {
    if (!onboardingOpen || currentPath === null) return;
    onboardingOpen = false;
    render();
    const target = returnFocus;
    returnFocus = null;
    (target ?? toggle).focus();
  }

  function choosePath(path: AiSupportPath, fromOnboarding: boolean) {
    currentPath = path;
    if (!writeAiSupportPath(storage, path)) {
      options.onNotice?.(
        "This choice works now, but Proof of Thought could not save it for the next launch.",
        "error",
      );
    }
    onboardingOpen = false;
    if (fromOnboarding) sidebarOpen = path !== "basic";
    render();
    void directEdits.refresh();
    if (fromOnboarding) {
      queueMicrotask(() => {
        if (sidebarOpen) closeButton.focus();
        else focusEditorOrToggle();
      });
    }
  }

  function open() {
    if (onboardingOpen) return;
    if (currentPath === null) {
      if (onboardingReady) showOnboardingIfNeeded();
      return;
    }
    sidebarOpen = true;
    render();
    queueMicrotask(() => closeButton.focus());
  }

  function close() {
    if (!sidebarOpen) return;
    sidebarOpen = false;
    render();
    toggle.focus();
  }

  function showOnboardingIfNeeded(): boolean {
    onboardingReady = true;
    if (currentPath !== null || onboardingOpen) return false;
    returnFocus = root.activeElement instanceof HTMLElement
      ? root.activeElement
      : null;
    onboardingOpen = true;
    sidebarOpen = false;
    render();
    queueMicrotask(() => {
      onboardingDialog.scrollTop = 0;
      onboardingTitle.focus({ preventScroll: true });
    });
    return true;
  }

  listen(toggle, "click", () => (sidebarOpen ? close() : open()));
  listen(closeButton, "click", close);
  listen(onboardingClose, "click", closeOnboarding);
  listen(onboarding, "mousedown", (event) => {
    if (event.target === onboarding) closeOnboarding();
  });
  for (const button of pathButtons) {
    const value = button.dataset.aiMode;
    if (!isAiSupportPath(value)) continue;
    listen(button, "click", () => choosePath(value, onboarding.contains(button)));
  }
  listen(window, "storage", (event) => {
    if (event.key !== AI_SUPPORT_PATH_STORAGE_KEY) return;
    const path = readAiSupportPath(storage);
    if (path === null) return;
    const focused = root.activeElement;
    const hidFocusedSurface = focused instanceof Node &&
      (onboarding.contains(focused) || sidebar.contains(focused));
    currentPath = path;
    onboardingOpen = false;
    sidebarOpen = false;
    render();
    if (hidFocusedSurface) queueMicrotask(focusEditorOrToggle);
  });
  listen(root, "keydown", (event) => {
    if (event.key === "Escape") {
      if (onboardingOpen) {
        event.preventDefault();
        event.stopImmediatePropagation();
        closeOnboarding();
      } else if (sidebarOpen) {
        event.preventDefault();
        event.stopImmediatePropagation();
        close();
      }
      return;
    }
    if (!onboardingOpen || event.key !== "Tab") return;
    const controls = [
      ...onboarding.querySelectorAll<HTMLElement>(
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

  render();

  return {
    isOpen: () => sidebarOpen || onboardingOpen || directEdits.isPrompting(),
    path: () => currentPath,
    isChoosingPath: () => onboardingOpen,
    showOnboardingIfNeeded,
    setConnectionCommand: reviewers.setExecutable,
    setReviewerApi: reviewers.setApi,
    setDirectEditApi: directEdits.setApi,
    setCurrentDocument(context) {
      reviewers.setDocument(context);
      chat?.setDocument(context);
      directEdits.setDocument(context && { id: context.id, title: context.title });
    },
    open,
    close,
    destroy() {
      for (const dispose of disposers.splice(0)) dispose();
      reviewers.destroy();
      chat?.destroy();
      providers?.destroy();
      directEdits.destroy();
      setBackgroundBlocked(false);
      delete root.documentElement.dataset.aiSupportPath;
    },
  };
}
