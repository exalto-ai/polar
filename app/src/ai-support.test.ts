import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installAiSupport } from "./ai-support";

beforeEach(() => {
  document.body.innerHTML = `
    <button id="ai-support-toggle" aria-expanded="false"></button>
    <aside id="ai-support-sidebar" hidden>
      <button id="ai-sidebar-close"></button>
      <button id="reviewer-add"></button>
      <p id="reviewer-error" hidden></p>
      <p id="reviewer-empty"></p>
      <ul id="reviewer-list"></ul>
      <form id="reviewer-form" hidden>
        <h3 id="reviewer-form-title"></h3>
        <select id="reviewer-client"><option value="codex">Codex</option></select>
        <input id="reviewer-label" />
        <select id="reviewer-scope"><option value="current">Current</option><option value="all">All</option></select>
        <p id="reviewer-current"></p>
        <button id="reviewer-cancel" type="button"></button>
      </form>
      <section id="reviewer-setup" hidden>
        <p id="reviewer-setup-text"></p>
        <pre id="reviewer-setup-command"></pre>
        <button id="reviewer-copy"></button>
        <button id="reviewer-setup-done"></button>
      </section>
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

  it("accepts the native reviewer executable without exposing a token", () => {
    const controller = installAiSupport(document);
    controller.setConnectionCommand("/Applications/Proof of Thought/thought-mcp-stdio");
    expect(document.body.textContent).not.toContain("Bearer");
    controller.destroy();
  });
});
