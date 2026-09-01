import type { ReviewerClient } from "./reviewer-setup";
import { reviewerClientName } from "./reviewer-setup";

export type DirectEditRequest = {
  request_id: string;
  connection_id: string;
  document_id: string;
  document_title: string;
  display_label: string;
  client: ReviewerClient;
  reported_model: string | null;
  requested_at: number;
  expires_at: number;
};

export type DirectEditGrant = {
  grant_id: string;
  connection_id: string;
  document_id: string;
  document_title: string;
  display_label: string;
  client: ReviewerClient;
  reported_model: string | null;
  granted_at: number;
};

export type DirectEditAccess = {
  requests: DirectEditRequest[];
  grants: DirectEditGrant[];
};

export type DirectEditDenial = {
  request_id: string;
  retry_at: number;
};

export type DirectEditApi = {
  listDirectEditAccess(): Promise<DirectEditAccess>;
  approveDirectEdit(documentId: string, requestId: string): Promise<DirectEditGrant>;
  denyDirectEdit(documentId: string, requestId: string): Promise<DirectEditDenial>;
  revokeDirectEdit(documentId: string, grantId: string): Promise<DirectEditGrant>;
};

export type DirectEditDocument = {
  id: string;
  title: string;
};

export type DirectEditAccessController = {
  isPrompting(): boolean;
  setApi(api: DirectEditApi | null): void;
  setDocument(document: DirectEditDocument | null): void;
  refresh(): Promise<void>;
  destroy(): void;
};

type Options = {
  api?: DirectEditApi | null;
  canPrompt?: () => boolean;
  onNotice?: (message: string, kind?: "info" | "error") => void;
  pollIntervalMs?: number;
};

const DEFAULT_POLL_INTERVAL_MS = 1_500;

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing direct edit element: ${selector}`);
  return value;
}

function oneLine(value: unknown): string {
  const message = value instanceof Error ? value.message : String(value);
  return message.replace(/[\r\n]+/g, " ").trim() || "Direct-edit access could not be updated.";
}

function sortedAccess(access: DirectEditAccess): DirectEditAccess {
  return {
    requests: [...access.requests].sort((left, right) =>
      left.requested_at - right.requested_at ||
      left.request_id.localeCompare(right.request_id)
    ),
    grants: [...access.grants].sort((left, right) =>
      left.granted_at - right.granted_at || left.grant_id.localeCompare(right.grant_id)
    ),
  };
}

function compactDocumentId(value: string): string {
  return value.length <= 20
    ? value
    : `${value.slice(0, 8)}…${value.slice(-6)}`;
}

/**
 * Own the editor-only approval surface for session-scoped direct editing.
 * Suggestions remain the default. A grant is visible globally and ends when
 * its authenticated AI session ends or the user revokes it.
 */
export function installDirectEditAccess(
  root: Document,
  options: Options = {},
): DirectEditAccessController {
  const toggle = required<HTMLButtonElement>(root, "#ai-support-toggle");
  const active = required<HTMLElement>(root, "#direct-edit-active");
  const activeList = required<HTMLUListElement>(root, "#direct-edit-grant-list");
  const activeError = required<HTMLElement>(root, "#direct-edit-active-error");
  const backdrop = required<HTMLElement>(root, "#direct-edit-prompt");
  const dialog = required<HTMLElement>(backdrop, '[role="alertdialog"]');
  const title = required<HTMLElement>(backdrop, "#direct-edit-prompt-title");
  const meta = required<HTMLElement>(backdrop, "#direct-edit-prompt-meta");
  const promptError = required<HTMLElement>(backdrop, "#direct-edit-prompt-error");
  const keep = required<HTMLButtonElement>(backdrop, "#direct-edit-keep-suggestions");
  const allow = required<HTMLButtonElement>(backdrop, "#direct-edit-allow");
  const canPrompt = options.canPrompt ?? (() => true);
  const pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
  const originalToggleText = toggle.textContent ?? "AI support";
  const originalToggleLabel = toggle.getAttribute("aria-label");
  const blockedElements = new Set<HTMLElement>();
  const disposers: Array<() => void> = [];
  const busyGrants = new Set<string>();
  let api = options.api ?? null;
  let currentDocument: DirectEditDocument | null = null;
  let access: DirectEditAccess = { requests: [], grants: [] };
  let prompt: DirectEditRequest | null = null;
  let promptAction: "allow" | "deny" | null = null;
  let returnFocus: HTMLElement | null = null;
  let destroyed = false;
  let contextVersion = 0;
  let actionVersion = 0;
  let listRequest = 0;
  let appliedListRequest = 0;
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let listFailed = false;
  let renderedGrantSignature = "";

  function listen(target: EventTarget, event: string, listener: EventListener) {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function setBackgroundBlocked(blocked: boolean) {
    if (blocked) {
      for (const element of root.body.children) {
        if (element === backdrop || !(element instanceof HTMLElement)) continue;
        if (!element.hasAttribute("inert")) {
          element.setAttribute("inert", "");
          blockedElements.add(element);
        }
      }
      return;
    }
    for (const element of blockedElements) element.removeAttribute("inert");
    blockedElements.clear();
  }

  function restoreFocus() {
    const target = returnFocus;
    returnFocus = null;
    const fallback = root.querySelector<HTMLElement>("#editor .tiptap") ?? toggle;
    queueMicrotask(() => {
      const destination = target?.isConnected && !target.closest("[inert]")
        ? target
        : fallback;
      destination.focus();
    });
  }

  function closePrompt(restore = true) {
    if (backdrop.hidden) return;
    backdrop.hidden = true;
    prompt = null;
    promptAction = null;
    dialog.removeAttribute("aria-busy");
    promptError.hidden = true;
    setBackgroundBlocked(false);
    if (restore) restoreFocus();
    else returnFocus = null;
  }

  function promptMeta(request: DirectEditRequest): string {
    const model = request.reported_model?.trim();
    return `${reviewerClientName(request.client)} · ${model ? `${model} (reported)` : "Model not reported"}`;
  }

  function openPrompt(request: DirectEditRequest) {
    if (prompt?.request_id === request.request_id && !backdrop.hidden) return;
    const replacingPrompt = !backdrop.hidden;
    const previousReturnFocus = returnFocus;
    if (replacingPrompt) closePrompt(false);
    prompt = request;
    promptAction = null;
    returnFocus = replacingPrompt
      ? previousReturnFocus
      : root.activeElement instanceof HTMLElement
        ? root.activeElement
        : null;
    const documentTitle = request.document_title?.trim() || currentDocument?.title.trim();
    title.textContent = documentTitle
      ? `Allow ${request.display_label} to edit “${documentTitle}” directly?`
      : `Allow ${request.display_label} to edit directly?`;
    meta.textContent = promptMeta(request);
    promptError.hidden = true;
    keep.disabled = false;
    allow.disabled = false;
    keep.textContent = "Keep suggestions";
    allow.textContent = "Allow direct editing";
    backdrop.hidden = false;
    setBackgroundBlocked(true);
    queueMicrotask(() => {
      dialog.scrollTop = 0;
      keep.focus({ preventScroll: true });
    });
  }

  function grantMeta(grant: DirectEditGrant): string {
    const isCurrent = currentDocument?.id === grant.document_id;
    const documentTitle = grant.document_title?.trim() ||
      (isCurrent ? currentDocument?.title.trim() : "");
    const location = documentTitle
      ? `${isCurrent ? "This document" : "Document"}: ${documentTitle}`
      : `Document ID ${compactDocumentId(grant.document_id)}`;
    const model = grant.reported_model?.trim();
    return [
      location,
      reviewerClientName(grant.client),
      model ? `${model} (reported)` : "Model not reported",
    ].join(" · ");
  }

  function renderActive(force = false) {
    const grants = access.grants;
    const focused = root.activeElement;
    const focusedRevokeLabel = focused instanceof HTMLButtonElement &&
        activeList.contains(focused)
      ? focused.getAttribute("aria-label")
      : null;
    const signature = JSON.stringify({
      grants: grants.map((grant) => ({ grant, meta: grantMeta(grant) })),
      busy: [...busyGrants].sort(),
    });
    if (!force && signature === renderedGrantSignature) return;
    renderedGrantSignature = signature;
    active.hidden = grants.length === 0;
    activeList.replaceChildren(
      ...grants.map((grant) => {
        const item = root.createElement("li");
        const details = root.createElement("div");
        const name = root.createElement("strong");
        const detailsText = root.createElement("small");
        const revoke = root.createElement("button");
        name.textContent = grant.display_label;
        detailsText.textContent = grantMeta(grant);
        details.append(name, detailsText);
        revoke.type = "button";
        revoke.textContent = busyGrants.has(grant.grant_id) ? "Revoking…" : "Revoke";
        revoke.disabled = busyGrants.has(grant.grant_id);
        revoke.setAttribute(
          "aria-label",
          `Revoke direct editing for ${grant.display_label}`,
        );
        revoke.addEventListener("click", () => void revokeGrant(grant));
        item.append(details, revoke);
        return item;
      }),
    );

    if (grants.length === 0) {
      toggle.textContent = originalToggleText;
      delete toggle.dataset.directEditActive;
      if (originalToggleLabel === null) toggle.removeAttribute("aria-label");
      else toggle.setAttribute("aria-label", originalToggleLabel);
      if (focusedRevokeLabel) queueMicrotask(() => toggle.focus());
      return;
    }
    toggle.textContent = grants.length === 1
      ? "Direct editing on"
      : `Direct editing on · ${grants.length}`;
    toggle.dataset.directEditActive = "true";
    toggle.setAttribute(
      "aria-label",
      `AI support, direct editing on for ${grants.length} ${grants.length === 1 ? "connection" : "connections"}`,
    );
    if (focusedRevokeLabel) {
      queueMicrotask(() => {
        const replacement = [...activeList.querySelectorAll<HTMLButtonElement>("button")]
          .find((button) =>
            !button.disabled && button.getAttribute("aria-label") === focusedRevokeLabel
          );
        const next = activeList.querySelector<HTMLButtonElement>("button:not([disabled])");
        (replacement ?? next ?? toggle).focus();
      });
    }
  }

  function renderPrompt() {
    const current = currentDocument;
    const next = current && canPrompt()
      ? access.requests.find((request) => request.document_id === current.id) ?? null
      : null;
    if (!next) {
      if (prompt && !promptAction) closePrompt();
      return;
    }
    if (!promptAction) openPrompt(next);
  }

  function render() {
    renderActive();
    renderPrompt();
  }

  function schedulePoll(delay = pollIntervalMs) {
    if (destroyed || api === null) return;
    if (pollTimer !== null) clearTimeout(pollTimer);
    pollTimer = setTimeout(() => {
      pollTimer = null;
      void refresh();
    }, delay);
  }

  async function refresh(): Promise<void> {
    if (destroyed || api === null) return;
    const selectedApi = api;
    const context = contextVersion;
    const actions = actionVersion;
    const request = ++listRequest;
    try {
      const value = sortedAccess(await selectedApi.listDirectEditAccess());
      if (
        destroyed || api !== selectedApi || contextVersion !== context ||
        actionVersion !== actions || request < appliedListRequest
      ) return;
      appliedListRequest = request;
      access = value;
      activeError.hidden = true;
      listFailed = false;
      render();
    } catch (reason) {
      if (
        !destroyed && api === selectedApi && contextVersion === context &&
        actionVersion === actions && !listFailed
      ) {
        listFailed = true;
        options.onNotice?.(
          `Could not refresh direct-edit access: ${oneLine(reason)}`,
          "error",
        );
      }
    } finally {
      if (!destroyed && api === selectedApi) schedulePoll();
    }
  }

  function setPromptBusy(action: "allow" | "deny") {
    promptAction = action;
    promptError.hidden = true;
    keep.disabled = true;
    allow.disabled = true;
    keep.textContent = action === "deny" ? "Keeping suggestions…" : "Keep suggestions";
    allow.textContent = action === "allow" ? "Allowing…" : "Allow direct editing";
    dialog.setAttribute("aria-busy", "true");
    dialog.focus({ preventScroll: true });
  }

  function resetPromptBusy() {
    promptAction = null;
    keep.disabled = false;
    allow.disabled = false;
    keep.textContent = "Keep suggestions";
    allow.textContent = "Allow direct editing";
    dialog.removeAttribute("aria-busy");
  }

  async function decide(action: "allow" | "deny") {
    const selectedApi = api;
    const selectedPrompt = prompt;
    if (!selectedApi || !selectedPrompt || promptAction) return;
    const context = contextVersion;
    actionVersion += 1;
    setPromptBusy(action);
    try {
      if (action === "allow") {
        const grant = await selectedApi.approveDirectEdit(
          selectedPrompt.document_id,
          selectedPrompt.request_id,
        );
        if (
          destroyed || api !== selectedApi || contextVersion !== context ||
          prompt?.request_id !== selectedPrompt.request_id
        ) return;
        access = {
          requests: access.requests.filter(
            (request) => request.request_id !== selectedPrompt.request_id,
          ),
          grants: sortedAccess({
            requests: [],
            grants: [
              ...access.grants.filter((value) => value.grant_id !== grant.grant_id),
              grant,
            ],
          }).grants,
        };
        closePrompt();
        options.onNotice?.(`Direct editing is on for ${grant.display_label}.`);
      } else {
        await selectedApi.denyDirectEdit(
          selectedPrompt.document_id,
          selectedPrompt.request_id,
        );
        if (
          destroyed || api !== selectedApi || contextVersion !== context ||
          prompt?.request_id !== selectedPrompt.request_id
        ) return;
        access = {
          ...access,
          requests: access.requests.filter(
            (request) => request.request_id !== selectedPrompt.request_id,
          ),
        };
        closePrompt();
        options.onNotice?.("Suggestions remain on for this AI connection.");
      }
      renderActive();
    } catch (reason) {
      if (
        !destroyed && api === selectedApi && contextVersion === context &&
        prompt?.request_id === selectedPrompt.request_id
      ) {
        promptError.textContent = oneLine(reason);
        promptError.hidden = false;
        resetPromptBusy();
        keep.focus();
      }
    } finally {
      actionVersion += 1;
      schedulePoll();
    }
  }

  async function revokeGrant(grant: DirectEditGrant) {
    const selectedApi = api;
    if (!selectedApi || busyGrants.has(grant.grant_id)) return;
    actionVersion += 1;
    busyGrants.add(grant.grant_id);
    activeError.hidden = true;
    renderActive();
    let revoked = false;
    try {
      await selectedApi.revokeDirectEdit(grant.document_id, grant.grant_id);
      if (destroyed || api !== selectedApi) return;
      access = {
        ...access,
        grants: access.grants.filter((value) => value.grant_id !== grant.grant_id),
      };
      revoked = true;
      options.onNotice?.(`Direct editing was revoked for ${grant.display_label}.`);
    } catch (reason) {
      if (!destroyed && api === selectedApi) {
        activeError.textContent = oneLine(reason);
        activeError.hidden = false;
      }
    } finally {
      busyGrants.delete(grant.grant_id);
      actionVersion += 1;
      if (!destroyed && api === selectedApi) {
        renderActive();
        if (revoked) {
          const next = activeList.querySelector<HTMLButtonElement>("button:not([disabled])");
          (next ?? toggle).focus();
        } else {
          [...activeList.querySelectorAll<HTMLButtonElement>("button")]
            .find((button) =>
              button.getAttribute("aria-label") ===
                `Revoke direct editing for ${grant.display_label}`
            )
            ?.focus();
        }
        schedulePoll();
      }
    }
  }

  listen(keep, "click", () => void decide("deny"));
  listen(allow, "click", () => void decide("allow"));
  listen(root, "keydown", (event) => {
    const keyboard = event as KeyboardEvent;
    if (backdrop.hidden) return;
    if (keyboard.key === "Escape") {
      keyboard.preventDefault();
      keyboard.stopImmediatePropagation();
      void decide("deny");
      return;
    }
    if (keyboard.key !== "Tab") return;
    if (promptAction) {
      keyboard.preventDefault();
      dialog.focus({ preventScroll: true });
      return;
    }
    const controls = [keep, allow].filter((button) => !button.disabled);
    if (controls.length === 0) return;
    const current = controls.indexOf(root.activeElement as HTMLButtonElement);
    const next = keyboard.shiftKey
      ? (current <= 0 ? controls.length : current) - 1
      : (current + 1) % controls.length;
    keyboard.preventDefault();
    controls[next].focus();
  });

  render();
  if (api) void refresh();

  return {
    isPrompting: () => !backdrop.hidden,
    setApi(value) {
      api = value;
      contextVersion += 1;
      actionVersion += 1;
      if (pollTimer !== null) clearTimeout(pollTimer);
      pollTimer = null;
      if (api) {
        void refresh();
      } else {
        access = { requests: [], grants: [] };
        busyGrants.clear();
        if (prompt) closePrompt();
        renderActive(true);
      }
    },
    setDocument(value) {
      const documentChanged = currentDocument?.id !== value?.id;
      const titleChanged = currentDocument?.title !== value?.title;
      if (!documentChanged && !titleChanged) return;
      currentDocument = value;
      if (!documentChanged) {
        renderActive();
        return;
      }
      contextVersion += 1;
      if (prompt) closePrompt(false);
      render();
      if (api) void refresh();
    },
    refresh,
    destroy() {
      destroyed = true;
      contextVersion += 1;
      if (pollTimer !== null) clearTimeout(pollTimer);
      for (const dispose of disposers.splice(0)) dispose();
      closePrompt(false);
      access = { requests: [], grants: [] };
      busyGrants.clear();
      renderActive(true);
      setBackgroundBlocked(false);
    },
  };
}
