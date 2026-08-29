import { writeClipboardText } from "./clipboard";
import type { ProChatBridge } from "./pro-chat-bridge";
import { installProChat, type ProChatDocumentContext } from "./pro-chat";
import type { ProProviderBridge } from "./pro-provider-bridge";
import { installProProvider, type ProProviderController } from "./pro-provider";
import {
  installReviewerConnections,
  type ReviewerApi,
} from "./reviewer-connections";

type AiSupportOptions = {
  copyText?: (text: string) => Promise<void>;
  reviewerApi?: ReviewerApi | null;
  providerBridge?: ProProviderBridge | null;
  chatBridge?: ProChatBridge | null;
  openExternal?: (url: string) => Promise<void>;
  onChatResponseCopied?: () => void;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type AiSupportController = {
  isOpen(): boolean;
  setConnectionCommand(command: string): void;
  setReviewerApi(api: ReviewerApi | null): void;
  setCurrentDocument(context: ProChatDocumentContext | null): void;
  open(): void;
  close(): void;
  destroy(): void;
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing AI support element: ${selector}`);
  return value;
}

/**
 * Own the shared sidebar only. Features rendered inside it keep their own
 * state and controllers.
 */
export function installAiSupport(
  root: Document,
  options: AiSupportOptions = {},
): AiSupportController {
  const copyText = options.copyText ?? writeClipboardText;
  const toggle = required<HTMLButtonElement>(root, "#ai-support-toggle");
  const sidebar = required<HTMLElement>(root, "#ai-support-sidebar");
  const closeButton = required<HTMLButtonElement>(root, "#ai-sidebar-close");
  const disposers: Array<() => void> = [];
  const reviewers = installReviewerConnections(root, {
    api: options.reviewerApi,
    copyText,
    onNotice: options.onNotice,
  });
  let providers: ProProviderController | null = null;
  const chat = root.querySelector("#pro-chat-view")
    ? installProChat(root, {
        bridge: options.chatBridge,
        onOpenSettings: () => root.querySelector("#ai-pro-panel")?.scrollIntoView(),
        onBusyChange: (provider) => providers?.setChatBusy(provider),
        copyText,
        onResponseCopied: options.onChatResponseCopied,
        onNotice: options.onNotice,
      })
    : null;
  providers = root.querySelector("#ai-pro-panel")
    ? installProProvider(root, {
        bridge: options.providerBridge,
        openExternal: options.openExternal,
        onNotice: options.onNotice,
        onConfigurationsChange: () => void chat?.refreshCapabilities(),
      })
    : null;

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
  function listen(
    target: Document | HTMLElement,
    event: string,
    listener: EventListener,
  ) {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function render() {
    const open = !sidebar.hidden;
    toggle.setAttribute("aria-expanded", String(open));
    reviewers.setOpen(open);
    providers?.setActive(open);
    chat?.setActive(open);
  }

  function open() {
    sidebar.hidden = false;
    render();
    queueMicrotask(() => closeButton.focus());
  }

  function close() {
    if (sidebar.hidden) return;
    sidebar.hidden = true;
    render();
    toggle.focus();
  }

  listen(toggle, "click", () => (sidebar.hidden ? open() : close()));
  listen(closeButton, "click", close);
  listen(root, "keydown", (event) => {
    if (event.key !== "Escape" || sidebar.hidden) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    close();
  });

  render();

  return {
    isOpen: () => !sidebar.hidden,
    setConnectionCommand: reviewers.setExecutable,
    setReviewerApi: reviewers.setApi,
    setCurrentDocument(context) {
      reviewers.setDocument(context);
      chat?.setDocument(context);
    },
    open,
    close,
    destroy() {
      for (const dispose of disposers.splice(0)) dispose();
      reviewers.destroy();
      chat?.cancelActive();
      chat?.destroy();
      providers?.destroy();
    },
  };
}
