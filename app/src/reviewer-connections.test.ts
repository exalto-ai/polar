import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import markup from "../index.html?raw";
import type {
  CreateReviewerConnection,
  ReviewerBridge,
  ReviewerConnection,
  ReviewerStatus,
  UpdateReviewerConnection,
} from "./reviewer-bridge";
import { installReviewerConnections } from "./reviewer-connections";

function fixture(): void {
  document.body.innerHTML = markup.match(/<body>([\s\S]*?)<\/body>/)?.[1] ?? "";
}

function connection(
  id: string,
  status: ReviewerStatus = "configured",
  overrides: Partial<ReviewerConnection> = {},
): ReviewerConnection {
  return {
    id,
    client: "chatgpt",
    provider: "openai",
    display_label: "Same name",
    status,
    permissions: {
      document_scope: "current",
      can_read: true,
      can_edit: false,
      can_create: false,
      can_trash: false,
      document_ids: ["doc-1"],
    },
    revision: 1,
    created_at: Number(id.replace(/\D/g, "")) || 1,
    first_connected_at: null,
    last_seen_at: null,
    failure_code: status === "failed" ? "credential_missing" : null,
    revoked_at: status === "revoked" ? 1_700_000_000 : null,
    reported_model: null,
    ...overrides,
  };
}

function statefulBridge(initial: ReviewerConnection[] = []) {
  let state = [...initial];
  const list = vi.fn(async () => [...state]);
  const create = vi.fn(async (input: CreateReviewerConnection) => {
    const created = connection(`connection-${state.length + 1}`, "configured", {
      client: input.client,
      provider: input.client.startsWith("claude") ? "anthropic" : "openai",
      display_label: input.display_label,
      permissions: input.permissions,
    });
    state.push(created);
    return created;
  });
  const update = vi.fn(async (id: string, input: UpdateReviewerConnection) => {
    const current = state.find((value) => value.id === id)!;
    const updated = {
      ...current,
      ...(input.display_label === undefined ? {} : { display_label: input.display_label }),
      ...(input.permissions === undefined ? {} : { permissions: input.permissions }),
      revision: current.revision + 1,
    };
    state = state.map((value) => (value.id === id ? updated : value));
    return updated;
  });
  const reset = vi.fn(async (id: string, _expectedRevision: number) => {
    const current = state.find((value) => value.id === id)!;
    const updated = { ...current, status: "disconnected" as const, revision: current.revision + 1 };
    state = state.map((value) => (value.id === id ? updated : value));
    return updated;
  });
  const revoke = vi.fn(async (id: string, _expectedRevision: number) => {
    const current = state.find((value) => value.id === id)!;
    const updated = {
      ...current,
      status: "revoked" as const,
      revision: current.revision + 1,
      revoked_at: 1_700_000_100,
    };
    state = state.map((value) => (value.id === id ? updated : value));
    return updated;
  });
  return {
    bridge: { list, create, update, reset, revoke } satisfies ReviewerBridge,
    list,
    create,
    update,
    reset,
    revoke,
    setState(next: ReviewerConnection[]) {
      state = [...next];
    },
  };
}

function action(label: string, id?: string): HTMLButtonElement {
  const match = [
    ...document.querySelectorAll<HTMLButtonElement>("button[data-reviewer-action]"),
  ].find(
    (candidate) =>
      candidate.textContent === label && (id === undefined || candidate.dataset.reviewerId === id),
  );
  if (!match) throw new Error(`missing action ${label}`);
  return match;
}

async function openAndLoad(bridge: ReviewerBridge) {
  const controller = installReviewerConnections(document, {
    bridge,
    pollIntervalMs: 60_000,
  });
  controller.setDocumentContext({ id: "doc-1", title: "Draft" });
  controller.setStdioExecutable(
    "/Applications/Proof of Thought.app/Contents/MacOS/thought-mcp-stdio",
  );
  controller.setSidebarOpen(true);
  await controller.refresh();
  return controller;
}

beforeEach(fixture);
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe("reviewer connection manager", () => {
  it("renders all five text statuses and keeps same-name reviewers separate by ID", async () => {
    const records = ([
      "configured",
      "connected",
      "disconnected",
      "failed",
      "revoked",
    ] as const).map((status, index) => connection(`connection-${index + 1}`, status));
    const state = statefulBridge(records);
    const controller = await openAndLoad(state.bridge);

    const activeCards = document.querySelectorAll("#reviewer-list .reviewer-card");
    const removedCards = document.querySelectorAll("#reviewer-revoked-list .reviewer-card");
    expect(activeCards).toHaveLength(4);
    expect(removedCards).toHaveLength(1);
    expect([...activeCards].map((card) => card.getAttribute("data-reviewer-id"))).toEqual([
      "connection-1",
      "connection-2",
      "connection-3",
      "connection-4",
    ]);
    expect(document.querySelector("#reviewer-manager")?.textContent).toContain("Ready to connect");
    expect(document.querySelector("#reviewer-manager")?.textContent).toContain("Connected");
    expect(document.querySelector("#reviewer-manager")?.textContent).toContain("Not connected");
    expect(document.querySelector("#reviewer-manager")?.textContent).toContain("Needs attention");
    expect(document.querySelector("#reviewer-manager")?.textContent).toContain("Access removed");
    controller.destroy();
  });

  it("explains every failure code accepted by the backend schema", async () => {
    const records = [
      connection("connection-1", "failed", { failure_code: "credential_missing" }),
      connection("connection-2", "failed", { failure_code: "credential_store" }),
      connection("connection-3", "failed", { failure_code: "protocol" }),
      connection("connection-4", "failed", { failure_code: "transport" }),
    ];
    const controller = await openAndLoad(statefulBridge(records).bridge);
    const copy = document.querySelector("#reviewer-list")?.textContent ?? "";

    expect(copy).toContain("Local access needs to be reset");
    expect(copy).toContain("could not use this saved connection");
    expect(copy).toContain("needs updated setup");
    expect(copy).toContain("Check its setup and try again");
    expect(copy).not.toContain("credential_invalid");
    expect(copy).not.toContain("permission_denied");
    controller.destroy();
  });

  it("labels the saved route and latest model without claiming app identity", async () => {
    const record = connection("connection-1", "connected", {
      client: "codex",
      display_label: "Copy editor",
      reported_model: "gpt-example",
    });
    const controller = await openAndLoad(statefulBridge([record]).bridge);
    const card = document.querySelector('[data-reviewer-id="connection-1"]')?.textContent ?? "";

    expect(card).toContain("Configured for Codex");
    expect(card).toContain("Last model reported by a tool: gpt-example");
    expect(card).not.toContain("gpt-example (reported)");
    controller.destroy();
  });

  it("creates a current-document reviewer and generates setup from its stable ID", async () => {
    const state = statefulBridge();
    const copied = vi.fn(async () => {});
    const controller = installReviewerConnections(document, {
      bridge: state.bridge,
      copyText: copied,
      pollIntervalMs: 60_000,
    });
    controller.setDocumentContext({ id: "doc-1", title: "Draft" });
    controller.setStdioExecutable("/Applications/Proof of Thought.app/Contents/MacOS/thought-mcp-stdio");
    controller.setSidebarOpen(true);
    await controller.refresh();

    document.querySelector<HTMLButtonElement>("#reviewer-add")!.click();
    expect(document.querySelector("#reviewer-add-form")?.textContent).toContain(
      "Reviewer connections are read-only",
    );
    const codex = document.querySelector<HTMLInputElement>(
      'input[name="reviewer-client"][value="codex"]',
    )!;
    codex.checked = true;
    codex.dispatchEvent(new Event("change", { bubbles: true }));
    document.querySelector<HTMLInputElement>("#reviewer-display-label")!.value = "Writing coach";
    controller.setDocumentContext({ id: "doc-2", title: "Another draft" });
    expect(document.querySelector("#reviewer-current-document-label")?.textContent).toContain(
      "Draft",
    );
    document.querySelector<HTMLFormElement>("#reviewer-add-form")!.dispatchEvent(
      new SubmitEvent("submit", { bubbles: true, cancelable: true }),
    );

    await vi.waitFor(() => expect(state.create).toHaveBeenCalledTimes(1));
    expect(state.create).toHaveBeenCalledWith({
      client: "codex",
      display_label: "Writing coach",
      permissions: {
        document_scope: "current",
        can_read: true,
        can_edit: false,
        can_create: false,
        can_trash: false,
        document_ids: ["doc-1"],
      },
    });
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#reviewer-setup")!.hidden).toBe(false);
    });
    const command = document.querySelector("#reviewer-setup-command")!.textContent!;
    expect(command).toContain("--connection connection-1");
    expect(command).not.toMatch(/bearer|secret|token|Writing coach/i);
    document.querySelector<HTMLButtonElement>("#reviewer-setup-copy")!.click();
    await vi.waitFor(() => expect(copied).toHaveBeenCalledWith(command));
    controller.destroy();
  });

  it("atomically updates reviewer names and access", async () => {
    const state = statefulBridge([connection("connection-1")]);
    const controller = await openAndLoad(state.bridge);

    expect(document.querySelector<HTMLButtonElement>("#reviewer-add")!.hidden).toBe(false);
    expect(document.querySelector("#reviewer-list")?.textContent).toContain("Same name");

    action("Rename").click();
    const rename = document.querySelector<HTMLFormElement>(
      'form[data-reviewer-form="rename"]',
    )!;
    rename.querySelector<HTMLInputElement>('input[name="display_label"]')!.value = "Research coach";
    rename.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));
    await vi.waitFor(() => expect(state.update).toHaveBeenCalledTimes(1));
    expect(state.update).toHaveBeenLastCalledWith("connection-1", {
      expected_revision: 1,
      display_label: "Research coach",
    });

    await vi.waitFor(() => expect(action("Change access", "connection-1")).toBeTruthy());
    action("Change access", "connection-1").click();
    const access = document.querySelector<HTMLFormElement>(
      'form[data-reviewer-form="permissions"]',
    )!;
    const all = access.querySelector<HTMLInputElement>(
      'input[name="reviewer-document-scope"][value="all"]',
    )!;
    all.checked = true;
    all.dispatchEvent(new Event("change", { bubbles: true }));
    access.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await vi.waitFor(() => expect(state.update).toHaveBeenCalledTimes(2));
    expect(state.update).toHaveBeenLastCalledWith("connection-1", {
      expected_revision: 2,
      permissions: {
        document_scope: "all",
        can_read: true,
        can_edit: false,
        can_create: false,
        can_trash: false,
        document_ids: [],
      },
    });
    controller.destroy();
  });

  it("keeps saved document access unless the user explicitly moves it", async () => {
    const state = statefulBridge([connection("connection-1")]);
    const controller = await openAndLoad(state.bridge);
    controller.setDocumentContext({ id: "doc-2", title: "Second draft" });

    action("Change access", "connection-1").click();
    let access = document.querySelector<HTMLFormElement>(
      'form[data-reviewer-form="permissions"]',
    )!;
    const preserved = access.querySelector<HTMLInputElement>(
      'input[name="reviewer-document-scope"][value="current"]',
    )!;
    expect(preserved.checked).toBe(true);
    expect(preserved.closest("label")?.textContent).toContain(
      "Keep access to the saved document",
    );
    expect(
      access.querySelector<HTMLInputElement>(
        'input[name="reviewer-document-scope"][value="open"]',
      ),
    ).not.toBeNull();
    access.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await vi.waitFor(() => expect(state.update).toHaveBeenCalledTimes(1));
    expect(state.update).toHaveBeenLastCalledWith("connection-1", {
      expected_revision: 1,
      permissions: {
        document_scope: "current",
        can_read: true,
        can_edit: false,
        can_create: false,
        can_trash: false,
        document_ids: ["doc-1"],
      },
    });

    await vi.waitFor(() => expect(action("Change access", "connection-1")).toBeTruthy());
    action("Change access", "connection-1").click();
    access = document.querySelector<HTMLFormElement>(
      'form[data-reviewer-form="permissions"]',
    )!;
    const move = access.querySelector<HTMLInputElement>(
      'input[name="reviewer-document-scope"][value="open"]',
    )!;
    move.checked = true;
    move.dispatchEvent(new Event("change", { bubbles: true }));
    access.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true }));

    await vi.waitFor(() => expect(state.update).toHaveBeenCalledTimes(2));
    expect(state.update).toHaveBeenLastCalledWith("connection-1", {
      expected_revision: 2,
      permissions: {
        document_scope: "current",
        can_read: true,
        can_edit: false,
        can_create: false,
        can_trash: false,
        document_ids: ["doc-2"],
      },
    });
    controller.destroy();
  });

  it("requires confirmations for credential reset and revocation, with modal focus containment", async () => {
    const state = statefulBridge([connection("connection-1", "disconnected")]);
    const controller = await openAndLoad(state.bridge);

    action("Reset connection").click();
    expect(document.querySelector<HTMLElement>("#reviewer-reset-warning")!.hidden).toBe(false);
    expect(state.reset).not.toHaveBeenCalled();
    document.querySelector<HTMLButtonElement>("#reviewer-reset-confirm")!.click();
    await vi.waitFor(() => expect(state.reset).toHaveBeenCalledWith("connection-1", 1));

    action("Remove access").click();
    await Promise.resolve();
    const backdrop = document.querySelector<HTMLElement>("#reviewer-revoke-backdrop")!;
    const cancel = document.querySelector<HTMLButtonElement>("#reviewer-revoke-cancel")!;
    const confirm = document.querySelector<HTMLButtonElement>("#reviewer-revoke-confirm")!;
    expect(backdrop.hidden).toBe(false);
    expect(document.querySelector("#reviewer-revoke-name")?.textContent).toBe("Same name");
    expect(document.activeElement).toBe(cancel);
    expect(state.revoke).not.toHaveBeenCalled();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement).toBe(confirm);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(backdrop.hidden).toBe(true);
    expect(state.revoke).not.toHaveBeenCalled();
    action("Remove access").click();
    await Promise.resolve();
    confirm.click();
    await vi.waitFor(() => expect(state.revoke).toHaveBeenCalledWith("connection-1", 2));
    expect(backdrop.hidden).toBe(true);
    expect(document.querySelector("#reviewer-revoked-list")?.textContent).toContain("Access removed");
    expect(document.activeElement).toBe(
      document.querySelector("#reviewer-revoked-section > summary"),
    );
    controller.destroy();
  });

  it("restores focus when polling revokes the reviewer open in setup", async () => {
    const state = statefulBridge([connection("connection-1", "connected")]);
    const controller = await openAndLoad(state.bridge);

    action("Reset connection", "connection-1").click();
    await Promise.resolve();
    expect(document.activeElement).toBe(
      document.querySelector("#reviewer-reset-cancel"),
    );

    state.setState([
      connection("connection-1", "revoked", {
        revision: 2,
        revoked_at: 1_700_000_100,
      }),
    ]);
    await controller.refresh();

    expect(document.querySelector<HTMLElement>("#reviewer-setup")!.hidden).toBe(true);
    expect(document.activeElement).toBe(
      document.querySelector("#reviewer-revoked-section > summary"),
    );
    controller.destroy();
  });

  it("keeps a revocation failure visible inside the modal", async () => {
    const state = statefulBridge([connection("connection-1")]);
    state.revoke.mockRejectedValueOnce(new Error("revision changed"));
    const controller = await openAndLoad(state.bridge);

    action("Remove access").click();
    document.querySelector<HTMLButtonElement>("#reviewer-revoke-confirm")!.click();
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#reviewer-revoke-error")!.hidden).toBe(false);
    });
    expect(document.querySelector("#reviewer-revoke-error")?.textContent).toContain(
      "revision changed",
    );
    expect(document.querySelector<HTMLElement>("#reviewer-revoke-backdrop")!.hidden).toBe(false);
    controller.destroy();
  });

  it("polls only while open, never overlaps, and ignores a response after closing", async () => {
    vi.useFakeTimers();
    let resolveFirst: ((connections: ReviewerConnection[]) => void) | null = null;
    const first = new Promise<ReviewerConnection[]>((resolve) => {
      resolveFirst = resolve;
    });
    const list = vi
      .fn<() => Promise<ReviewerConnection[]>>()
      .mockReturnValueOnce(first)
      .mockResolvedValueOnce([connection("connection-2", "connected")]);
    const bridge = {
      list,
      create: vi.fn(),
      update: vi.fn(),
      reset: vi.fn(),
      revoke: vi.fn(),
    } satisfies ReviewerBridge;
    const controller = installReviewerConnections(document, {
      bridge,
      pollIntervalMs: 1_000,
    });
    controller.setDocumentContext({ id: "doc-1", title: "Draft" });

    await vi.advanceTimersByTimeAsync(5_000);
    expect(list).not.toHaveBeenCalled();
    controller.setSidebarOpen(true);
    expect(list).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(list).toHaveBeenCalledTimes(1);

    controller.setSidebarOpen(false);
    resolveFirst!([connection("stale", "failed")]);
    await Promise.resolve();
    expect(document.querySelector('[data-reviewer-id="stale"]')).toBeNull();

    controller.setSidebarOpen(true);
    await controller.refresh();
    expect(list).toHaveBeenCalledTimes(2);
    expect(document.querySelector('[data-reviewer-id="connection-2"]')).not.toBeNull();
    controller.destroy();
  });

  it("announces only real status changes, not unchanged polls", async () => {
    const state = statefulBridge([connection("connection-1", "configured")]);
    const controller = await openAndLoad(state.bridge);
    const live = document.querySelector<HTMLElement>("#reviewer-live")!;
    expect(live.textContent).toBe("");

    await controller.refresh();
    expect(live.textContent).toBe("");
    const manage = document.querySelector<HTMLDetailsElement>(".reviewer-actions")!;
    manage.open = true;
    action("Rename").focus();
    state.setState([connection("connection-1", "connected")]);
    await controller.refresh();
    await Promise.resolve();
    expect(live.textContent).toBe("Same name is now Connected.");
    expect(document.activeElement?.textContent).toBe("Rename");
    expect(document.querySelector<HTMLDetailsElement>(".reviewer-actions")!.open).toBe(true);
    controller.destroy();
  });
});
