import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  installDirectEditAccess,
  type DirectEditAccess,
  type DirectEditApi,
  type DirectEditGrant,
  type DirectEditRequest,
} from "./direct-edit-access";

function request(overrides: Partial<DirectEditRequest> = {}): DirectEditRequest {
  return {
    request_id: "request-one",
    connection_id: "connection-one",
    document_id: "doc-one",
    document_title: "Draft",
    display_label: "Writing coach",
    client: "codex",
    reported_model: "gpt-reported",
    requested_at: 10,
    expires_at: 300_010,
    ...overrides,
  };
}

function grant(overrides: Partial<DirectEditGrant> = {}): DirectEditGrant {
  return {
    grant_id: "grant-one",
    connection_id: "connection-one",
    document_id: "doc-one",
    document_title: "Draft",
    display_label: "Writing coach",
    client: "codex",
    reported_model: "gpt-reported",
    granted_at: 20,
    ...overrides,
  };
}

function api(access: DirectEditAccess): DirectEditApi {
  return {
    listDirectEditAccess: vi.fn().mockResolvedValue(access),
    approveDirectEdit: vi.fn().mockResolvedValue(grant()),
    denyDirectEdit: vi.fn().mockResolvedValue({
      request_id: "request-one",
      retry_at: 300_010,
    }),
    revokeDirectEdit: vi.fn().mockImplementation(
      async (_documentId: string, grantId: string) =>
        grantId === "grant-two"
          ? grant({
              grant_id: "grant-two",
              connection_id: "connection-two",
              document_id: "doc-two",
              display_label: "Second reviewer",
            })
          : grant(),
    ),
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((accept, decline) => {
    resolve = accept;
    reject = decline;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  document.body.innerHTML = `
    <header><button id="ai-support-toggle">AI support</button></header>
    <div id="editor"><div class="tiptap" tabindex="0"></div></div>
    <aside id="ai-support-sidebar">
      <section id="direct-edit-active" hidden>
        <h3>Direct editing on</h3>
        <p>These AI connections can edit immediately while they remain open. Direct edits do not have Accept or Reject. Suggestions still do.</p>
        <p id="direct-edit-active-error" role="alert" hidden></p>
        <ul id="direct-edit-grant-list"></ul>
      </section>
    </aside>
    <div id="direct-edit-prompt" hidden>
      <section role="alertdialog" tabindex="-1">
        <h2 id="direct-edit-prompt-title"></h2>
        <p id="direct-edit-prompt-meta"></p>
        <p id="direct-edit-prompt-description">This connection wants to edit this document directly while it remains open. Its app, model, and identity are reported, not verified.</p>
        <p id="direct-edit-prompt-default">Keep suggestions leaves the current workflow unchanged: every proposed change appears with Accept and Reject.</p>
        <p id="direct-edit-prompt-error" role="alert" hidden></p>
        <button id="direct-edit-keep-suggestions"></button>
        <button id="direct-edit-allow"></button>
      </section>
    </div>
  `;
});

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("direct-edit approval", () => {
  it("keeps suggestions as the focused default and restores editor focus after denial", async () => {
    const directEdit = api({ requests: [request()], grants: [] });
    const editor = document.querySelector<HTMLElement>(".tiptap")!;
    editor.focus();
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);

    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(false);
    });
    expect(document.querySelector("#direct-edit-prompt-title")!.textContent).toBe(
      "Allow Writing coach to edit “Draft” directly?",
    );
    expect(document.querySelector("#direct-edit-prompt-meta")!.textContent).toBe(
      "Codex · gpt-reported (reported)",
    );
    expect(document.querySelector("#direct-edit-prompt")!.textContent).toContain(
      "app, model, and identity are reported, not verified",
    );
    expect(document.querySelector("#direct-edit-prompt")!.textContent).toContain(
      "Accept and Reject",
    );
    const keep = document.querySelector<HTMLButtonElement>(
      "#direct-edit-keep-suggestions",
    )!;
    expect(document.activeElement).toBe(keep);
    expect(document.querySelector("#editor")!.hasAttribute("inert")).toBe(true);

    keep.click();
    await vi.waitFor(() => {
      expect(directEdit.denyDirectEdit).toHaveBeenCalledWith("doc-one", "request-one");
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(true);
      expect(document.activeElement).toBe(editor);
    });
    expect(document.querySelector("#editor")!.hasAttribute("inert")).toBe(false);
    controller.destroy();
  });

  it("traps focus and treats Escape as Keep suggestions", async () => {
    const directEdit = api({ requests: [request()], grants: [] });
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    const keep = document.querySelector<HTMLButtonElement>(
      "#direct-edit-keep-suggestions",
    )!;
    const allow = document.querySelector<HTMLButtonElement>("#direct-edit-allow")!;
    await vi.waitFor(() => expect(document.activeElement).toBe(keep));

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement).toBe(allow);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement).toBe(keep);
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true }),
    );
    expect(document.activeElement).toBe(allow);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));

    await vi.waitFor(() => {
      expect(directEdit.denyDirectEdit).toHaveBeenCalledTimes(1);
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(true);
    });
    controller.destroy();
  });

  it("keeps focus inside the dialog while an approval is pending", async () => {
    const approval = deferred<DirectEditGrant>();
    const directEdit = api({ requests: [request()], grants: [] });
    vi.mocked(directEdit.approveDirectEdit).mockReturnValueOnce(approval.promise);
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    const allow = document.querySelector<HTMLButtonElement>("#direct-edit-allow")!;
    const dialog = document.querySelector<HTMLElement>('[role="alertdialog"]')!;
    await vi.waitFor(() => expect(allow.disabled).toBe(false));

    allow.click();
    expect(dialog.getAttribute("aria-busy")).toBe("true");
    expect(document.activeElement).toBe(dialog);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", bubbles: true }));
    expect(document.activeElement).toBe(dialog);

    approval.resolve(grant());
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(true);
    });
    controller.destroy();
  });

  it("does not duplicate a grant observed by polling before approval returns", async () => {
    const approval = deferred<DirectEditGrant>();
    const directEdit = api({ requests: [request()], grants: [] });
    vi.mocked(directEdit.approveDirectEdit).mockReturnValueOnce(approval.promise);
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(false);
    });
    document.querySelector<HTMLButtonElement>("#direct-edit-allow")!.click();
    vi.mocked(directEdit.listDirectEditAccess).mockResolvedValueOnce({
      requests: [],
      grants: [grant()],
    });
    await controller.refresh();
    expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(1);

    approval.resolve(grant());
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(true);
    });
    expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(1);
    controller.destroy();
  });

  it("keeps a failed safe decision visible and actionable", async () => {
    const directEdit = api({ requests: [request()], grants: [] });
    vi.mocked(directEdit.denyDirectEdit).mockRejectedValueOnce(new Error("daemon unavailable"));
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    const keep = document.querySelector<HTMLButtonElement>(
      "#direct-edit-keep-suggestions",
    )!;
    await vi.waitFor(() => expect(document.activeElement).toBe(keep));
    keep.click();

    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(false);
      expect(document.querySelector("#direct-edit-prompt-error")!.textContent).toBe(
        "daemon unavailable",
      );
      expect(keep.disabled).toBe(false);
      expect(document.activeElement).toBe(keep);
    });
    controller.destroy();
  });

  it("closes a pending prompt when the editor API is removed", async () => {
    const directEdit = api({ requests: [request()], grants: [] });
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(false);
    });

    controller.setApi(null);
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(true);
      expect(document.querySelector("#editor")!.hasAttribute("inert")).toBe(false);
    });
    controller.destroy();
  });
});

describe("active direct-edit access", () => {
  it("shows session-scoped access globally and revokes it in one click", async () => {
    const directEdit = api({ requests: [request()], grants: [] });
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    await vi.waitFor(() => {
      expect(document.querySelector<HTMLElement>("#direct-edit-prompt")!.hidden).toBe(false);
    });

    document.querySelector<HTMLButtonElement>("#direct-edit-allow")!.click();
    await vi.waitFor(() => {
      expect(directEdit.approveDirectEdit).toHaveBeenCalledWith("doc-one", "request-one");
      expect(document.querySelector<HTMLElement>("#direct-edit-active")!.hidden).toBe(false);
    });
    expect(document.querySelector("#ai-support-toggle")!.textContent).toBe(
      "Direct editing on",
    );
    expect(document.querySelector("#direct-edit-active")!.textContent).toContain(
      "Direct edits do not have Accept or Reject. Suggestions still do.",
    );
    expect(document.querySelector("#direct-edit-grant-list")!.textContent).toContain(
      "This document: Draft · Codex · gpt-reported (reported)",
    );
    expect(document.querySelector("#direct-edit-active")!.textContent).not.toMatch(
      /\b\d{1,2}:\d{2}\b/,
    );

    document.querySelector<HTMLButtonElement>(
      '[aria-label="Revoke direct editing for Writing coach"]',
    )!.click();
    await vi.waitFor(() => {
      expect(directEdit.revokeDirectEdit).toHaveBeenCalledWith("doc-one", "grant-one");
      expect(document.querySelector<HTMLElement>("#direct-edit-active")!.hidden).toBe(true);
    });
    expect(document.querySelector("#ai-support-toggle")!.textContent).toBe("AI support");
    controller.destroy();
  });

  it("lists every active session and distinguishes another document", async () => {
    const directEdit = api({
      requests: [],
      grants: [
        grant(),
        grant({
          grant_id: "grant-two",
          connection_id: "connection-two",
          document_id: "doc-two",
          document_title: "Research notes",
          display_label: "Second reviewer",
          client: "claude-code",
          reported_model: null,
          granted_at: 30,
        }),
      ],
    });
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);

    await vi.waitFor(() => {
      expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(2);
    });
    expect(document.querySelector("#ai-support-toggle")!.textContent).toBe(
      "Direct editing on · 2",
    );
    expect(document.querySelector("#direct-edit-grant-list")!.textContent).toContain(
      "Document: Research notes · Claude Code · Model not reported",
    );

    document.querySelector<HTMLButtonElement>(
      '[aria-label="Revoke direct editing for Second reviewer"]',
    )!.click();
    await vi.waitFor(() => {
      expect(directEdit.revokeDirectEdit).toHaveBeenCalledWith("doc-two", "grant-two");
      expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(1);
    });
    expect(document.activeElement).toBe(
      document.querySelector('[aria-label="Revoke direct editing for Writing coach"]'),
    );
    controller.destroy();
  });

  it("preserves revoke focus across unchanged polls and clears stale API state", async () => {
    const activeGrant = grant();
    const directEdit = api({ requests: [], grants: [activeGrant] });
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);

    await vi.waitFor(() => {
      expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(1);
    });
    const before = document.querySelector<HTMLButtonElement>(
      '[aria-label="Revoke direct editing for Writing coach"]',
    )!;
    before.focus();
    await controller.refresh();

    const after = document.querySelector<HTMLButtonElement>(
      '[aria-label="Revoke direct editing for Writing coach"]',
    )!;
    expect(after).toBe(before);
    expect(document.activeElement).toBe(before);

    controller.setApi(null);
    expect(document.querySelector<HTMLElement>("#direct-edit-active")!.hidden).toBe(true);
    expect(document.querySelector("#ai-support-toggle")!.textContent).toBe("AI support");
    controller.destroy();
  });

  it("preserves the active list when only the current title changes", async () => {
    const directEdit = api({ requests: [], grants: [grant()] });
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "Draft" });
    controller.setApi(directEdit);
    await vi.waitFor(() => {
      expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(1);
    });
    const before = document.querySelector("#direct-edit-grant-list li");

    controller.setDocument({ id: "doc-one", title: "A newly typed heading" });

    expect(document.querySelector("#direct-edit-grant-list li")).toBe(before);
    controller.destroy();
  });

  it("lets the user retry a failed revoke after switching documents", async () => {
    const revocation = deferred<DirectEditGrant>();
    const directEdit = api({ requests: [], grants: [grant()] });
    vi.mocked(directEdit.revokeDirectEdit)
      .mockReturnValueOnce(revocation.promise)
      .mockResolvedValueOnce(grant());
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "First" });
    controller.setApi(directEdit);
    await vi.waitFor(() => {
      expect(document.querySelectorAll("#direct-edit-grant-list li")).toHaveLength(1);
    });
    document.querySelector<HTMLButtonElement>(
      '[aria-label="Revoke direct editing for Writing coach"]',
    )!.click();
    controller.setDocument({ id: "doc-two", title: "Second" });
    revocation.reject(new Error("daemon unavailable"));

    await vi.waitFor(() => {
      const retry = document.querySelector<HTMLButtonElement>(
        '[aria-label="Revoke direct editing for Writing coach"]',
      )!;
      expect(retry.disabled).toBe(false);
      expect(retry.textContent).toBe("Revoke");
    });
    document.querySelector<HTMLButtonElement>(
      '[aria-label="Revoke direct editing for Writing coach"]',
    )!.click();
    await vi.waitFor(() => {
      expect(directEdit.revokeDirectEdit).toHaveBeenCalledTimes(2);
    });
    controller.destroy();
  });
});

describe("direct-edit request races", () => {
  it("does not let an older document response replace the current request", async () => {
    const first = deferred<DirectEditAccess>();
    const second = deferred<DirectEditAccess>();
    const directEdit = api({ requests: [], grants: [] });
    vi.mocked(directEdit.listDirectEditAccess)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const controller = installDirectEditAccess(document, {
      pollIntervalMs: 60_000,
    });
    controller.setDocument({ id: "doc-one", title: "First" });
    controller.setApi(directEdit);
    controller.setDocument({ id: "doc-two", title: "Second" });

    second.resolve({
      requests: [request({
        request_id: "request-two",
        document_id: "doc-two",
        document_title: "Second",
        display_label: "Current reviewer",
      })],
      grants: [],
    });
    await vi.waitFor(() => {
      expect(document.querySelector("#direct-edit-prompt-title")!.textContent).toBe(
        "Allow Current reviewer to edit “Second” directly?",
      );
    });
    first.resolve({ requests: [request({ display_label: "Stale reviewer" })], grants: [] });
    await Promise.resolve();
    await Promise.resolve();

    expect(document.querySelector("#direct-edit-prompt-title")!.textContent).toBe(
      "Allow Current reviewer to edit “Second” directly?",
    );
    controller.destroy();
  });
});
