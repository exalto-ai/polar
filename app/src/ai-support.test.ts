import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AI_SUPPORT_STORAGE_KEY,
  installAiSupport,
  readAiSupportMode,
  safeLocalStorage,
  setupCommand,
  writeAiSupportMode,
} from "./ai-support";

function storage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => values.delete(key),
    setItem: (key, value) => values.set(key, String(value)),
  };
}

function fixture() {
  document.body.innerHTML = `
    <header class="chrome"></header>
    <button id="ai-support-toggle"></button>
    <div class="workspace"></div>
    <div id="other-surface"></div>
    <div id="preblocked-surface" inert></div>
    <div id="close-prompt" hidden></div>
    <aside id="ai-support-sidebar" hidden>
      <button id="ai-sidebar-close"></button>
      <button id="change-ai-mode"></button>
      <h2 id="ai-mode-title"></h2>
      <p id="ai-mode-description"></p>
      <span id="ai-mode-evidence"></span>
      <p id="ai-mode-cost"></p>
      <section id="ai-connect-panel">
        <button data-ai-client="chatgpt"></button>
        <button data-ai-client="codex"></button>
        <button data-ai-client="claude-desktop"></button>
        <button data-ai-client="claude-code"></button>
        <p id="ai-client-setup"></p>
        <p id="ai-client-caveat"></p>
        <code id="ai-connection-command"></code>
        <button id="copy-ai-command"></button>
      </section>
      <section id="ai-basic-panel"></section>
    </aside>
    <div id="ai-onboarding" hidden>
      <section role="dialog">
        <button id="ai-onboarding-close"></button>
        <h1 id="ai-onboarding-title" tabindex="-1">How would you like to work?</h1>
        <div id="ai-startup-error" hidden>
          <span id="ai-startup-error-message"></span>
        </div>
        <button data-ai-mode="connect">Connect</button>
        <button data-ai-mode="basic">Basic</button>
        <button disabled>Pro</button>
        <details><summary>Evidence details</summary></details>
      </section>
    </div>
  `;
}

beforeEach(fixture);
afterEach(() => {
  document.body.replaceChildren();
  document.documentElement.removeAttribute("data-ai-support-mode");
  vi.restoreAllMocks();
});

describe("AI support preference", () => {
  it("round-trips only the versioned supported modes", () => {
    const values = storage();
    expect(readAiSupportMode(values)).toBeNull();
    expect(writeAiSupportMode(values, "connect")).toBe(true);
    expect(readAiSupportMode(values)).toBe("connect");

    values.setItem(AI_SUPPORT_STORAGE_KEY, JSON.stringify({ version: 2, mode: "connect" }));
    expect(readAiSupportMode(values)).toBeNull();
    values.setItem(AI_SUPPORT_STORAGE_KEY, "not json");
    expect(readAiSupportMode(values)).toBeNull();
  });

  it("continues safely when storage is unavailable", () => {
    const denied = storage();
    denied.getItem = () => {
      throw new Error("denied");
    };
    denied.setItem = () => {
      throw new Error("denied");
    };
    expect(readAiSupportMode(denied)).toBeNull();
    expect(writeAiSupportMode(denied, "basic")).toBe(false);
    expect(readAiSupportMode(null)).toBeNull();
    expect(writeAiSupportMode(null, "basic")).toBe(false);
  });

  it("handles a browser that denies access to local storage", () => {
    const denied = {
      get localStorage(): Storage {
        throw new Error("denied");
      },
    };
    expect(safeLocalStorage(denied)).toBeNull();
  });
});

describe("client setup commands", () => {
  const stdio = "/Applications/Proof of Thought.app/Contents/MacOS/thought-mcp-stdio";

  it("uses the raw command for ChatGPT desktop", () => {
    expect(setupCommand("chatgpt", stdio)).toBe(stdio);
  });

  it("builds valid Codex and Claude Code commands", () => {
    expect(setupCommand("codex", stdio)).toBe(
      `codex mcp add proof-of-thought -- '${stdio}'`,
    );
    expect(setupCommand("claude-code", stdio)).toBe(
      `claude mcp add --scope user proof-of-thought -- '${stdio}'`,
    );
  });

  it("shell-quotes an executable path containing an apostrophe", () => {
    const quoted = "/Applications/Proof's Thought.app/Contents/MacOS/thought-mcp-stdio";
    expect(setupCommand("codex", quoted)).toBe(
      `codex mcp add proof-of-thought -- '/Applications/Proof'"'"'s Thought.app/Contents/MacOS/thought-mcp-stdio'`,
    );
  });

  it("does not pretend the Claude Desktop installer exists", () => {
    expect(setupCommand("claude-desktop", stdio)).toBeNull();
    expect(setupCommand("codex", "  ")).toBeNull();
  });
});

describe("AI support surfaces", () => {
  it("requires a choice on first launch and recommends Connect", async () => {
    const values = storage();
    const controller = installAiSupport(document, { storage: values });
    const modal = document.querySelector<HTMLElement>("#ai-onboarding")!;
    const close = document.querySelector<HTMLButtonElement>("#ai-onboarding-close")!;

    expect(modal.hidden).toBe(false);
    expect(controller.isChoosingMode()).toBe(true);
    expect(close.hidden).toBe(true);
    expect(document.querySelector(".workspace")?.hasAttribute("inert")).toBe(true);
    expect(document.querySelector("#other-surface")?.hasAttribute("inert")).toBe(true);
    let choice: string | null = null;
    void controller.whenInitialChoiceMade().then((mode) => {
      choice = mode;
    });
    await Promise.resolve();
    expect(document.activeElement?.textContent).toBe("How would you like to work?");
    expect(choice).toBeNull();

    document.querySelector<HTMLButtonElement>('[data-ai-mode="connect"]')!.click();
    await Promise.resolve();
    expect(controller.mode()).toBe("connect");
    expect(choice).toBe("connect");
    expect(controller.isChoosingMode()).toBe(false);
    expect(modal.hidden).toBe(true);
    expect(document.querySelector<HTMLElement>("#ai-support-sidebar")!.hidden).toBe(false);
    expect(readAiSupportMode(values)).toBe("connect");
    expect(document.querySelector(".workspace")?.hasAttribute("inert")).toBe(false);
    expect(document.querySelector("#other-surface")?.hasAttribute("inert")).toBe(false);
    expect(document.querySelector("#preblocked-surface")?.hasAttribute("inert")).toBe(true);
    expect(document.activeElement).toBe(
      document.querySelector<HTMLButtonElement>("#ai-sidebar-close"),
    );
    controller.destroy();
  });

  it("includes disclosure controls in the onboarding focus loop", async () => {
    const controller = installAiSupport(document, { storage: storage() });
    await Promise.resolve();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement?.textContent).toBe("Connect");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement?.textContent).toBe("Basic");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement?.textContent).toBe("Evidence details");
    controller.destroy();
  });

  it("allows a keyboard-only first-launch choice", async () => {
    const controller = installAiSupport(document, { storage: storage() });
    const basic = document.querySelector<HTMLButtonElement>('[data-ai-mode="basic"]')!;
    basic.focus();
    basic.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    await expect(controller.whenInitialChoiceMade()).resolves.toBe("basic");
    expect(controller.mode()).toBe("basic");
    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(true);
    controller.destroy();
  });

  it("applies a preference chosen in another window", async () => {
    const values = storage();
    const controller = installAiSupport(document, { storage: values });
    writeAiSupportMode(values, "basic");
    window.dispatchEvent(
      new StorageEvent("storage", {
        key: AI_SUPPORT_STORAGE_KEY,
        newValue: values.getItem(AI_SUPPORT_STORAGE_KEY),
      }),
    );

    await expect(controller.whenInitialChoiceMade()).resolves.toBe("basic");
    expect(controller.mode()).toBe("basic");
    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(true);
    controller.destroy();
  });

  it("restores Basic without replaying onboarding and can change modes", () => {
    const values = storage();
    writeAiSupportMode(values, "basic");
    const changed = vi.fn();
    const controller = installAiSupport(document, { storage: values, onModeChange: changed });

    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(true);
    expect(document.querySelector("#ai-mode-title")?.textContent).toBe("Basic recording");
    controller.openSidebar();
    document.querySelector<HTMLButtonElement>("#change-ai-mode")!.click();
    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(false);
    expect(document.querySelector<HTMLButtonElement>("#ai-onboarding-close")!.hidden).toBe(false);
    document.querySelector<HTMLButtonElement>('[data-ai-mode="connect"]')!.click();

    expect(controller.mode()).toBe("connect");
    expect(changed).toHaveBeenLastCalledWith("connect");
    controller.destroy();
  });

  it("shows startup failures inside the visible setup surface", () => {
    const values = storage();
    writeAiSupportMode(values, "basic");
    const controller = installAiSupport(document, { storage: values });

    controller.setStartupError("incompatible daemon protocol");
    controller.showModePicker();

    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(false);
    expect(document.querySelector<HTMLElement>("#ai-startup-error")!.hidden).toBe(false);
    expect(document.querySelector("#ai-startup-error-message")?.textContent).toBe(
      "incompatible daemon protocol",
    );
    expect(document.querySelector("#ai-connection-command")?.textContent).toContain(
      "unavailable until Proof of Thought starts",
    );
    expect(document.querySelector<HTMLButtonElement>("#copy-ai-command")!.disabled).toBe(true);
    expect(document.querySelector("#copy-ai-command")?.textContent).toBe("Unavailable");
    controller.destroy();
  });

  it("suppresses an already loaded setup command after startup fails", () => {
    const values = storage();
    writeAiSupportMode(values, "connect");
    const copyText = vi.fn(async () => {});
    const controller = installAiSupport(document, { storage: values, copyText });
    const copy = document.querySelector<HTMLButtonElement>("#copy-ai-command")!;
    controller.setConnectionCommand("thought-mcp-stdio");
    expect(copy.disabled).toBe(false);

    controller.setStartupError("connection failed");
    copy.click();

    expect(copy.disabled).toBe(true);
    expect(document.querySelector("#ai-connection-command")?.textContent).toContain(
      "unavailable until Proof of Thought starts",
    );
    expect(copyText).not.toHaveBeenCalled();
    controller.destroy();
  });

  it("keeps a fatal first-launch error visible after a mode choice", async () => {
    const controller = installAiSupport(document, { storage: storage() });
    controller.setStartupError("daemon unavailable");

    document.querySelector<HTMLButtonElement>('[data-ai-mode="basic"]')!.click();

    await expect(controller.whenInitialChoiceMade()).resolves.toBe("basic");
    expect(controller.mode()).toBe("basic");
    expect(controller.isChoosingMode()).toBe(true);
    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(false);
    expect(document.querySelector<HTMLButtonElement>("#ai-onboarding-close")!.hidden).toBe(true);
    expect(controller.dismissModePicker()).toBe(false);
    controller.destroy();
  });

  it("contains Escape inside onboarding even when the required choice stays open", () => {
    const controller = installAiSupport(document, { storage: storage() });
    const leaked = vi.fn();
    document.addEventListener("keydown", leaked);
    const event = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });

    document.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(true);
    expect(leaked).not.toHaveBeenCalled();
    expect(controller.isChoosingMode()).toBe(true);
    document.removeEventListener("keydown", leaked);
    controller.destroy();
  });

  it("releases an optional setup sheet before another native modal takes over", () => {
    const values = storage();
    writeAiSupportMode(values, "basic");
    const controller = installAiSupport(document, { storage: values });
    const closePrompt = document.querySelector<HTMLElement>("#close-prompt")!;

    controller.showModePicker();
    expect(closePrompt.hasAttribute("inert")).toBe(true);
    expect(controller.dismissModePicker()).toBe(true);
    expect(closePrompt.hasAttribute("inert")).toBe(false);
    expect(controller.isChoosingMode()).toBe(false);
    controller.destroy();
  });

  it("does not dismiss the required first-launch choice", () => {
    const controller = installAiSupport(document, { storage: storage() });

    expect(controller.dismissModePicker()).toBe(false);
    expect(controller.isChoosingMode()).toBe(true);
    controller.destroy();
  });

  it("copies the command for the selected guided client", async () => {
    const values = storage();
    writeAiSupportMode(values, "connect");
    const copyText = vi.fn(async () => {});
    const controller = installAiSupport(document, { storage: values, copyText });
    controller.setConnectionCommand("thought-mcp-stdio");

    document.querySelector<HTMLButtonElement>('[data-ai-client="claude-code"]')!.click();
    expect(document.querySelector("#ai-connection-command")?.textContent).toBe(
      "claude mcp add --scope user proof-of-thought -- thought-mcp-stdio",
    );
    document.querySelector<HTMLButtonElement>("#copy-ai-command")!.click();
    await Promise.resolve();
    expect(copyText).toHaveBeenCalledWith(
      "claude mcp add --scope user proof-of-thought -- thought-mcp-stdio",
    );
    controller.destroy();
  });

  it("keeps the planned Claude Desktop path visibly unavailable", () => {
    const values = storage();
    writeAiSupportMode(values, "connect");
    const controller = installAiSupport(document, { storage: values });
    controller.setConnectionCommand("thought-mcp-stdio");

    document.querySelector<HTMLButtonElement>('[data-ai-client="claude-desktop"]')!.click();
    expect(document.querySelector<HTMLButtonElement>("#copy-ai-command")!.disabled).toBe(true);
    expect(document.querySelector("#ai-connection-command")?.textContent).toContain(
      "next connection PR",
    );
    controller.destroy();
  });

  it("does not claim that setup itself is a live connection", () => {
    const values = storage();
    writeAiSupportMode(values, "connect");
    const controller = installAiSupport(document, { storage: values });

    expect(document.querySelector("#ai-mode-evidence")?.textContent).toBe(
      "Setup choice, not connection proof",
    );
    controller.destroy();
  });
});
