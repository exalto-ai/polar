type AiSupportOptions = {
  copyText?: (text: string) => Promise<void>;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type AiSupportController = {
  isOpen(): boolean;
  setConnectionCommand(command: string): void;
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
  const toggle = required<HTMLButtonElement>(root, "#ai-support-toggle");
  const sidebar = required<HTMLElement>(root, "#ai-support-sidebar");
  const closeButton = required<HTMLButtonElement>(root, "#ai-sidebar-close");
  const command = required<HTMLElement>(root, "#stdio-command");
  const copyButton = required<HTMLButtonElement>(root, "#copy-command");
  const copyText = options.copyText ?? ((text: string) => navigator.clipboard.writeText(text));
  const disposers: Array<() => void> = [];
  let connectionCommand = "";

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
  listen(copyButton, "click", () => {
    if (!connectionCommand) return;
    void copyText(connectionCommand)
      .then(() => {
        copyButton.textContent = "Copied";
        window.setTimeout(() => (copyButton.textContent = "Copy"), 1200);
      })
      .catch(() => options.onNotice?.("Could not copy the agent command.", "error"));
  });

  render();

  return {
    isOpen: () => !sidebar.hidden,
    setConnectionCommand(value) {
      connectionCommand = value;
      command.textContent = value;
      copyButton.disabled = !value;
    },
    open,
    close,
    destroy() {
      for (const dispose of disposers.splice(0)) dispose();
    },
  };
}
