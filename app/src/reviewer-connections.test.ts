import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  installReviewerConnections,
  type ReviewerApi,
  type ReviewerConnection,
} from "./reviewer-connections";

beforeEach(() => {
  document.body.innerHTML = `
    <button id="reviewer-add"></button>
    <p id="reviewer-error" hidden></p>
    <p id="reviewer-empty"></p>
    <ul id="reviewer-list"></ul>
    <form id="reviewer-form" hidden>
      <h3 id="reviewer-form-title"></h3>
      <select id="reviewer-client"><option value="chatgpt">ChatGPT</option><option value="codex">Codex</option></select>
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
  `;
});

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("reviewer connections", () => {
  it("creates current-document access and exposes only a setup id", async () => {
    const saved: ReviewerConnection = {
      id: "abc123",
      client: "codex",
      provider: "openai",
      display_label: "Review",
      status: "configured",
      access: { document_scope: "current", document_id: "doc-1" },
      revision: 1,
      created_at: 10,
      last_seen_at: null,
      revoked_at: null,
      reported_model: null,
    };
    const api: ReviewerApi = {
      listReviewerConnections: vi.fn().mockResolvedValue([]),
      createReviewerConnection: vi.fn().mockResolvedValue(saved),
      updateReviewerConnection: vi.fn(),
      resetReviewerConnection: vi.fn(),
      revokeReviewerConnection: vi.fn(),
    };
    const controller = installReviewerConnections(document, { api });
    controller.setDocument({ id: "doc-1", title: "Draft" });
    controller.setExecutable("/Applications/Proof of Thought/shim");
    controller.setOpen(true);
    await Promise.resolve();

    document.querySelector<HTMLButtonElement>("#reviewer-add")!.click();
    document.querySelector<HTMLSelectElement>("#reviewer-client")!.value = "codex";
    document.querySelector<HTMLInputElement>("#reviewer-label")!.value = "Review";
    document
      .querySelector<HTMLFormElement>("#reviewer-form")!
      .dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));
    await Promise.resolve();
    await Promise.resolve();

    expect(api.createReviewerConnection).toHaveBeenCalledWith({
      client: "codex",
      display_label: "Review",
      access: { document_scope: "current", document_id: "doc-1" },
    });
    const command = document.querySelector("#reviewer-setup-command")!.textContent!;
    expect(command).toContain("--connection abc123");
    expect(command).not.toContain("Bearer");
    controller.destroy();
  });
});
