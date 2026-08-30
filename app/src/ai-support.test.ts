import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const reviewers = vi.hoisted(() => ({
  setSidebarOpen: vi.fn(),
  setStdioExecutable: vi.fn(),
  setBridge: vi.fn(),
  setDocumentContext: vi.fn(),
  destroy: vi.fn(),
}));

vi.mock("./reviewer-connections", () => ({
  installReviewerConnections: () => reviewers,
}));

import { installAiSupport } from "./ai-support";

beforeEach(() => {
  document.body.innerHTML = `
    <button id="ai-support-toggle" aria-expanded="false"></button>
    <aside id="ai-support-sidebar" hidden>
      <button id="ai-sidebar-close"></button>
    </aside>
  `;
  vi.clearAllMocks();
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
    expect(reviewers.setSidebarOpen).toHaveBeenLastCalledWith(true);
    expect(document.activeElement).toBe(
      document.querySelector("#ai-sidebar-close"),
    );

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(sidebar.hidden).toBe(true);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(toggle);
    controller.destroy();
  });

  it("delegates reviewer state without owning it", () => {
    const controller = installAiSupport(document);
    const bridge = {} as never;
    const context = {
      id: "doc-1",
      title: "Draft",
      snapshot: () => ({}),
      suggestionPosition: () => ({ kind: "end" } as const),
      waitUntilSaved: () => Promise.resolve(true),
    };

    controller.setConnectionCommand("thought-mcp-stdio");
    controller.setReviewerBridge(bridge);
    controller.setCurrentDocument(context);

    expect(reviewers.setStdioExecutable).toHaveBeenCalledWith("thought-mcp-stdio");
    expect(reviewers.setBridge).toHaveBeenCalledWith(bridge);
    expect(reviewers.setDocumentContext).toHaveBeenCalledWith(context);
    controller.destroy();
    expect(reviewers.destroy).toHaveBeenCalledOnce();
  });
});
