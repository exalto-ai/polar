import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installAiSupport } from "./ai-support";

beforeEach(() => {
  document.body.innerHTML = `
    <button id="ai-support-toggle" aria-expanded="false"></button>
    <aside id="ai-support-sidebar" hidden>
      <button id="ai-sidebar-close"></button>
      <pre id="stdio-command"></pre>
      <button id="copy-command"></button>
    </aside>
  `;
});

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("AI support sidebar", () => {
  it("opens without blocking the document and restores focus when closed", async () => {
    const controller = installAiSupport(document);
    const toggle = document.querySelector<HTMLButtonElement>("#ai-support-toggle")!;
    const sidebar = document.querySelector<HTMLElement>("#ai-support-sidebar")!;

    expect(controller.isOpen()).toBe(false);
    expect(document.querySelector("[inert]")).toBeNull();
    toggle.click();
    await Promise.resolve();

    expect(sidebar.hidden).toBe(false);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(
      document.querySelector("#ai-sidebar-close"),
    );

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(sidebar.hidden).toBe(true);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(toggle);
    controller.destroy();
  });

  it("copies the current connection command", async () => {
    const copyText = vi.fn().mockResolvedValue(undefined);
    const controller = installAiSupport(document, { copyText });
    const button = document.querySelector<HTMLButtonElement>("#copy-command")!;

    controller.setConnectionCommand("thought-mcp-stdio");
    expect(document.querySelector("#stdio-command")?.textContent).toBe(
      "thought-mcp-stdio",
    );
    expect(button.disabled).toBe(false);
    button.click();
    await Promise.resolve();

    expect(copyText).toHaveBeenCalledWith("thought-mcp-stdio");
    controller.destroy();
  });
});
