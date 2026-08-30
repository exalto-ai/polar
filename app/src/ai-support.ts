import { writeClipboardText } from "./clipboard";
import type { ProProviderBridge } from "./pro-provider-bridge";
import type { ProChatBridge } from "./pro-chat-bridge";
import { installProChat, type ProChatDocument } from "./pro-chat";
import { installProProvider } from "./pro-provider";
import {
  installReviewerConnections,
  type ReviewerApi,
} from "./reviewer-connections";

type AiSupportOptions = {
  copyText?: (text: string) => Promise<void>;
  reviewerApi?: ReviewerApi | null;
  providerBridge?: ProProviderBridge | null;
  chatBridge?: ProChatBridge | null;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type AiSupportController = {
  isOpen(): boolean;
  setConnectionCommand(command: string): void;
  setReviewerApi(api: ReviewerApi | null): void;
  setCurrentDocument(context: ProChatDocument | null): void;
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
  const chat = root.querySelector("#pro-chat")
    ? installProChat(root, { bridge: options.chatBridge })
    : null;
  const providers = root.querySelector("#provider-settings")
    ? installProProvider(root, {
        bridge: options.providerBridge,
        onNotice: options.onNotice,
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
    chat?.setActive(open);
    providers?.setActive(open);
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
      chat?.destroy();
      providers?.destroy();
    },
  };
}
