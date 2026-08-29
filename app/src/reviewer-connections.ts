import {
  permissionsForScope,
  type ReviewerBridge,
  type ReviewerConnection,
  type ReviewerDocumentScope,
  type ReviewerPermissions,
  type ReviewerStatus,
} from "./reviewer-bridge";
import {
  REVIEWER_CLIENTS,
  reviewerClient,
  reviewerSetupCommand,
  type ReviewerClient,
} from "./reviewer-setup";

export type ReviewerDocumentContext = { id: string; title: string };

export type ReviewerConnectionsOptions = {
  bridge?: ReviewerBridge | null;
  copyText?: (text: string) => Promise<void>;
  onNotice?: (message: string, kind?: "info" | "error") => void;
  pollIntervalMs?: number;
};

export type ReviewerConnectionsController = {
  isModalOpen(): boolean;
  dismissModal(): boolean;
  setBridge(bridge: ReviewerBridge | null): void;
  setSidebarOpen(open: boolean): void;
  setDocumentContext(context: ReviewerDocumentContext | null): void;
  setStdioExecutable(path: string): void;
  refresh(): Promise<void>;
  destroy(): void;
};

type EditKind = "rename" | "permissions";
type Editing = {
  id: string;
  kind: EditKind;
  expectedRevision: number;
  documentContext: ReviewerDocumentContext | null;
};

const STATUS_LABEL: Record<ReviewerStatus, string> = {
  configured: "Ready to connect",
  connected: "Connected",
  disconnected: "Not connected",
  failed: "Needs attention",
  revoked: "Access removed",
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing reviewer element: ${selector}`);
  return value;
}

function button(label: string, action: string, id: string): HTMLButtonElement {
  const value = document.createElement("button");
  value.type = "button";
  value.textContent = label;
  value.dataset.reviewerAction = action;
  value.dataset.reviewerId = id;
  return value;
}

function labelWithInput(
  input: HTMLInputElement,
  title: string,
  description?: string,
): HTMLLabelElement {
  const label = document.createElement("label");
  label.className = "reviewer-choice";
  const copy = document.createElement("span");
  const strong = document.createElement("strong");
  strong.textContent = title;
  copy.append(strong);
  if (description) {
    const small = document.createElement("small");
    small.textContent = description;
    copy.append(small);
  }
  label.append(input, copy);
  return label;
}

function checkbox(
  name: keyof Pick<
    ReviewerPermissions,
    "can_read" | "can_edit" | "can_create" | "can_trash"
  >,
  checked: boolean,
  title: string,
  description: string,
  disabled = false,
): HTMLLabelElement {
  const input = document.createElement("input");
  input.type = "checkbox";
  input.name = name;
  input.checked = checked;
  input.disabled = disabled;
  return labelWithInput(input, title, description);
}

export function installReviewerConnections(
  root: Document,
  options: ReviewerConnectionsOptions = {},
): ReviewerConnectionsController {
  const addButton = required<HTMLButtonElement>(root, "#reviewer-add");
  const loading = required<HTMLElement>(root, "#reviewer-loading");
  const errorBox = required<HTMLElement>(root, "#reviewer-error");
  const errorMessage = required<HTMLElement>(root, "#reviewer-error-message");
  const retry = required<HTMLButtonElement>(root, "#reviewer-retry");
  const live = required<HTMLElement>(root, "#reviewer-live");
  const empty = required<HTMLElement>(root, "#reviewer-empty");
  const list = required<HTMLUListElement>(root, "#reviewer-list");
  const revokedSection = required<HTMLDetailsElement>(root, "#reviewer-revoked-section");
  const revokedSummary = required<HTMLElement>(revokedSection, "summary");
  const revokedCount = required<HTMLElement>(root, "#reviewer-revoked-count");
  const revokedList = required<HTMLUListElement>(root, "#reviewer-revoked-list");

  const addFlow = required<HTMLElement>(root, "#reviewer-add-flow");
  const addHeading = required<HTMLElement>(root, "#reviewer-add-title");
  const addForm = required<HTMLFormElement>(root, "#reviewer-add-form");
  const addLabel = required<HTMLInputElement>(root, "#reviewer-display-label");
  const addSetup = required<HTMLElement>(root, "#reviewer-client-description");
  const addCaveat = required<HTMLElement>(root, "#reviewer-client-caveat");
  const currentDocumentLabel = required<HTMLElement>(root, "#reviewer-current-document-label");
  const addSubmit = required<HTMLButtonElement>(root, "#reviewer-add-submit");
  const addCancel = required<HTMLButtonElement>(root, "#reviewer-add-cancel");
  const clientInputs = [
    ...addForm.querySelectorAll<HTMLInputElement>("input[name='reviewer-client']"),
  ];

  const setupPanel = required<HTMLElement>(root, "#reviewer-setup");
  const setupHeading = required<HTMLElement>(root, "#reviewer-setup-title");
  const setupStatus = required<HTMLElement>(root, "#reviewer-setup-status");
  const setupInstructions = required<HTMLElement>(root, "#reviewer-setup-instructions");
  const setupCaveat = required<HTMLElement>(root, "#reviewer-setup-caveat");
  const setupCommand = required<HTMLElement>(root, "#reviewer-setup-command");
  const setupCopy = required<HTMLButtonElement>(root, "#reviewer-setup-copy");
  const setupReset = required<HTMLButtonElement>(root, "#reviewer-setup-reset");
  const setupDone = required<HTMLButtonElement>(root, "#reviewer-setup-done");
  const resetWarning = required<HTMLElement>(root, "#reviewer-reset-warning");
  const resetCancel = required<HTMLButtonElement>(root, "#reviewer-reset-cancel");
  const resetConfirm = required<HTMLButtonElement>(root, "#reviewer-reset-confirm");

  const revokeBackdrop = required<HTMLElement>(root, "#reviewer-revoke-backdrop");
  const revokeName = required<HTMLElement>(root, "#reviewer-revoke-name");
  const revokeError = required<HTMLElement>(root, "#reviewer-revoke-error");
  const revokeCancel = required<HTMLButtonElement>(root, "#reviewer-revoke-cancel");
  const revokeConfirm = required<HTMLButtonElement>(root, "#reviewer-revoke-confirm");

  const copyText = options.copyText ?? ((text: string) => navigator.clipboard.writeText(text));
  const pollIntervalMs = options.pollIntervalMs ?? 5_000;
  const disposers: Array<() => void> = [];
  const connections = new Map<string, ReviewerConnection>();
  const modalBlockedElements = new Set<HTMLElement>();

  let bridge = options.bridge ?? null;
  let sidebarOpen = false;
  let documentContext: ReviewerDocumentContext | null = null;
  let stdioExecutable = "";
  let loaded = false;
  let loadError: string | null = null;
  let pollTimer: number | null = null;
  let refreshInFlight: Promise<void> | null = null;
  let refreshQueued = false;
  let requestEpoch = 0;
  let renderedSignature = "";
  let editing: Editing | null = null;
  let addDocumentContext: ReviewerDocumentContext | null = null;
  let activeSetupId: string | null = null;
  let resetConfirmationOpen = false;
  let resetExpectedRevision: number | null = null;
  let revokeId: string | null = null;
  let revokeExpectedRevision: number | null = null;
  let revokeReturnFocus: HTMLElement | null = null;
  let destroyed = false;

  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    event: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ): void {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function friendlyError(error: unknown): string {
    const raw = error instanceof Error ? error.message : String(error);
    const line = raw.replace(/[\r\n]+/g, " ").trim() || "Unknown problem";
    return line.length > 180 ? `${line.slice(0, 177)}…` : line;
  }

  function schedulePoll(): void {
    if (!sidebarOpen || bridge === null || destroyed) return;
    if (pollTimer !== null) window.clearTimeout(pollTimer);
    pollTimer = window.setTimeout(() => {
      pollTimer = null;
      void refresh();
    }, pollIntervalMs);
  }

  function clearPoll(): void {
    if (pollTimer !== null) window.clearTimeout(pollTimer);
    pollTimer = null;
  }

  function connectionSignature(): string {
    return JSON.stringify(
      [...connections.values()]
        .sort((left, right) => left.id.localeCompare(right.id))
        .map((connection) => ({ ...connection })),
    );
  }

  function updateChrome(): void {
    addButton.disabled = bridge === null || documentContext === null;
    addButton.title = documentContext === null ? "Waiting for a document to open" : "";
    loading.hidden = bridge !== null && (loaded || loadError !== null);
    errorBox.hidden = loadError === null;
    errorMessage.textContent = loadError ?? "";
    const active = [...connections.values()].filter(({ status }) => status !== "revoked");
    const revoked = [...connections.values()].filter(({ status }) => status === "revoked");
    empty.hidden = !loaded || active.length !== 0;
    list.hidden = active.length === 0;
    revokedSection.hidden = revoked.length === 0;
    revokedCount.textContent = String(revoked.length);
    const addContext = addDocumentContext ?? documentContext;
    const title = addContext?.title.trim() || "Untitled";
    currentDocumentLabel.textContent = addContext
      ? `Only “${title}”`
      : "This document is still opening";
    const currentScope = addForm.querySelector<HTMLInputElement>(
      "input[name='reviewer-document-scope'][value='current']",
    );
    if (currentScope) currentScope.disabled = addContext === null;
    addSubmit.disabled = bridge === null || addContext === null;
  }

  function statusDescription(connection: ReviewerConnection): string | null {
    if (connection.status !== "failed") return null;
    switch (connection.failure_code) {
      case "credential_missing":
        return "Local access needs to be reset before this reviewer can reconnect.";
      case "credential_store":
        return "Proof of Thought could not use this saved connection. Reset it to reconnect.";
      case "protocol":
        return "This connection needs updated setup before the reviewer can reconnect.";
      case "transport":
        return "The reviewer could not connect. Check its setup and try again.";
      default:
        return "The reviewer could not connect. Continue setup or reset the connection.";
    }
  }

  function permissionSummary(connection: ReviewerConnection): string {
    const access = connection.permissions.document_scope === "all"
      ? "All documents"
      : connection.permissions.document_ids.length === 1 &&
          connection.permissions.document_ids[0] === documentContext?.id
        ? `This document: ${documentContext.title.trim() || "Untitled"}`
        : `${connection.permissions.document_ids.length} selected document${
            connection.permissions.document_ids.length === 1 ? "" : "s"
          }`;
    const abilities = [connection.permissions.can_read ? "read" : null]
      .filter((value): value is string => value !== null);
    return `${access} · ${abilities.length ? abilities.join(", ") : "no document actions"}`;
  }

  function formatTimestamp(value: number | null): string | null {
    if (value === null || !Number.isFinite(value) || value <= 0) return null;
    const milliseconds = value < 1_000_000_000_000 ? value * 1000 : value;
    const date = new Date(milliseconds);
    if (!Number.isFinite(date.valueOf())) return null;
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(date);
  }

  function renderPermissionsForm(connection: ReviewerConnection): HTMLFormElement {
    const form = document.createElement("form");
    form.className = "reviewer-inline-form";
    form.dataset.reviewerForm = "permissions";
    form.dataset.reviewerId = connection.id;
    form.dataset.expectedRevision = String(editing?.expectedRevision ?? connection.revision);

    const heading = document.createElement("h4");
    heading.textContent = "Change access";
    heading.tabIndex = -1;
    form.append(heading);

    const scope = document.createElement("fieldset");
    const scopeLegend = document.createElement("legend");
    scopeLegend.textContent = "Documents";
    scope.append(scopeLegend);
    const editContext = editing?.documentContext ?? documentContext;
    const editTitle = editContext?.title.trim() || "Untitled";
    const savedDocumentId = connection.permissions.document_scope === "current"
      ? connection.permissions.document_ids[0] ?? null
      : null;
    const savedDocumentIsOpen = savedDocumentId !== null && savedDocumentId === editContext?.id;
    const choices: ReadonlyArray<readonly [string, string, string]> = savedDocumentId !== null &&
        !savedDocumentIsOpen
      ? [
          [
            "current",
            "Keep access to the saved document",
            `Does not grant access to “${editTitle}”`,
          ],
          ...(editContext === null
            ? []
            : [[
                "open",
                `Move access to “${editTitle}”`,
                "Replaces access to the saved document",
              ] as const]),
          ["all", "All documents", "Includes documents created later"],
        ]
      : [
          ["current", `Only “${editTitle}”`, "Access stays with this document"],
          ["all", "All documents", "Includes documents created later"],
        ];
    for (const [value, title, description] of choices) {
      const input = document.createElement("input");
      input.type = "radio";
      input.name = "reviewer-document-scope";
      input.value = value;
      input.checked = value === connection.permissions.document_scope;
      input.disabled =
        (value === "current" && editContext === null && savedDocumentId === null) ||
        (value === "open" && editContext === null);
      scope.append(labelWithInput(input, title, description));
    }
    form.append(scope, permissionsFieldset());

    const actions = document.createElement("div");
    actions.className = "reviewer-form-actions";
    const cancel = button("Cancel", "cancel-edit", connection.id);
    const save = document.createElement("button");
    save.type = "submit";
    save.className = "primary-button compact";
    save.textContent = "Save access";
    actions.append(cancel, save);
    form.append(actions);
    return form;
  }

  function permissionsFieldset(): HTMLFieldSetElement {
    const fieldset = document.createElement("fieldset");
    const legend = document.createElement("legend");
    legend.textContent = "Allowed actions";
    fieldset.append(
      legend,
      checkbox(
        "can_read",
        true,
        "Read",
        "Required so the reviewer can work with allowed documents",
        true,
      ),
    );
    const note = document.createElement("p");
    note.className = "reviewer-permission-note";
    note.textContent = "Reviewer connections are read-only. Reviewable suggestions arrive in a later update.";
    fieldset.append(note);
    return fieldset;
  }

  function renderRenameForm(connection: ReviewerConnection): HTMLFormElement {
    const form = document.createElement("form");
    form.className = "reviewer-inline-form";
    form.dataset.reviewerForm = "rename";
    form.dataset.reviewerId = connection.id;
    form.dataset.expectedRevision = String(editing?.expectedRevision ?? connection.revision);
    const label = document.createElement("label");
    label.textContent = "Reviewer name";
    const input = document.createElement("input");
    input.type = "text";
    input.name = "display_label";
    input.maxLength = 80;
    input.required = true;
    input.value = connection.display_label;
    label.append(input);
    const actions = document.createElement("div");
    actions.className = "reviewer-form-actions";
    const cancel = button("Cancel", "cancel-edit", connection.id);
    const save = document.createElement("button");
    save.type = "submit";
    save.className = "primary-button compact";
    save.textContent = "Save name";
    actions.append(cancel, save);
    form.append(label, actions);
    return form;
  }

  function renderConnection(connection: ReviewerConnection, revoked: boolean): HTMLLIElement {
    const item = document.createElement("li");
    item.className = "reviewer-card";
    item.dataset.reviewerId = connection.id;

    const header = document.createElement("div");
    header.className = "reviewer-card-header";
    const identity = document.createElement("div");
    const name = document.createElement("h4");
    name.textContent = connection.display_label;
    const app = document.createElement("span");
    app.className = "reviewer-client";
    app.textContent = `Configured for ${reviewerClient(connection.client).name}`;
    identity.append(name, app);
    const status = document.createElement("span");
    status.className = "reviewer-status";
    status.dataset.status = connection.status;
    status.textContent = STATUS_LABEL[connection.status];
    header.append(identity, status);
    item.append(header);

    const access = document.createElement("p");
    access.className = "reviewer-access-summary";
    access.textContent = permissionSummary(connection);
    item.append(access);

    const metadata: string[] = [];
    if (connection.reported_model) {
      metadata.push(`Last model reported by a tool: ${connection.reported_model}`);
    }
    const seen = formatTimestamp(connection.last_seen_at);
    if (seen) metadata.push(`Last connected ${seen}`);
    if (metadata.length) {
      const meta = document.createElement("p");
      meta.className = "reviewer-metadata";
      meta.textContent = metadata.join(" · ");
      item.append(meta);
    }
    const failure = statusDescription(connection);
    if (failure) {
      const explanation = document.createElement("p");
      explanation.className = "reviewer-failure";
      explanation.textContent = failure;
      item.append(explanation);
    }

    if (revoked) {
      const revokedAt = formatTimestamp(connection.revoked_at);
      if (revokedAt) {
        const removed = document.createElement("p");
        removed.className = "reviewer-metadata";
        removed.textContent = `Removed ${revokedAt}`;
        item.append(removed);
      }
      return item;
    }

    const actions = document.createElement("details");
    actions.className = "reviewer-actions";
    const summary = document.createElement("summary");
    summary.textContent = "Manage";
    const actionList = document.createElement("div");
    actionList.className = "reviewer-action-list";
    const setupLabel = connection.status === "configured"
      ? "Continue setup"
      : connection.status === "connected"
        ? "View connection steps"
        : "Reconnect";
    actionList.append(
      button(setupLabel, "setup", connection.id),
      button("Rename", "rename", connection.id),
      button("Change access", "permissions", connection.id),
      button("Reset connection", "reset", connection.id),
      button("Remove access", "revoke", connection.id),
    );
    actions.append(summary, actionList);
    item.append(actions);

    if (editing?.id === connection.id) {
      item.append(
        editing.kind === "rename"
          ? renderRenameForm(connection)
          : renderPermissionsForm(connection),
      );
    }
    return item;
  }

  function renderLists(force = false): void {
    updateChrome();
    const signature = connectionSignature();
    if (!force && signature === renderedSignature) return;
    if (editing !== null && !force) return;
    const activeElement = root.activeElement instanceof HTMLElement
      ? root.activeElement
      : null;
    const activeCard = activeElement?.closest<HTMLElement>(".reviewer-card[data-reviewer-id]");
    const focusState = activeCard && list.contains(activeCard)
      ? {
          id: activeCard.dataset.reviewerId ?? "",
          action: activeElement?.dataset.reviewerAction ?? null,
          manage: activeElement?.matches(".reviewer-actions > summary") ?? false,
        }
      : null;
    const openActions = new Set(
      [...list.querySelectorAll<HTMLDetailsElement>(".reviewer-actions[open]")]
        .map((details) => details.closest<HTMLElement>("[data-reviewer-id]")?.dataset.reviewerId)
        .filter((id): id is string => Boolean(id)),
    );
    renderedSignature = signature;
    const sorted = [...connections.values()].sort(
      (left, right) => left.created_at - right.created_at || left.id.localeCompare(right.id),
    );
    list.replaceChildren(
      ...sorted
        .filter(({ status }) => status !== "revoked")
        .map((connection) => renderConnection(connection, false)),
    );
    revokedList.replaceChildren(
      ...sorted
        .filter(({ status }) => status === "revoked")
        .map((connection) => renderConnection(connection, true)),
    );
    for (const card of list.querySelectorAll<HTMLElement>(".reviewer-card[data-reviewer-id]")) {
      if (card.dataset.reviewerId && openActions.has(card.dataset.reviewerId)) {
        const details = card.querySelector<HTMLDetailsElement>(".reviewer-actions");
        if (details) details.open = true;
      }
    }
    updateChrome();
    if (focusState?.id) {
      queueMicrotask(() => {
        const target = focusState.action
          ? findAction(focusState.id, focusState.action)
          : focusState.manage
            ? [...list.querySelectorAll<HTMLElement>(".reviewer-card[data-reviewer-id]")]
                .find((card) => card.dataset.reviewerId === focusState.id)
                ?.querySelector<HTMLElement>(".reviewer-actions > summary") ?? null
            : null;
        if (target) target.focus();
        else if (connections.get(focusState.id)?.status === "revoked") revokedSummary.focus();
      });
    }
    if (editing) {
      const activeEdit = editing;
      queueMicrotask(() => {
        const item = [...list.querySelectorAll<HTMLElement>("[data-reviewer-id]")].find(
          (candidate) => candidate.dataset.reviewerId === activeEdit.id,
        );
        const focus = activeEdit.kind === "rename"
          ? item?.querySelector<HTMLInputElement>("input[name='display_label']")
          : item?.querySelector<HTMLElement>(".reviewer-inline-form h4");
        focus?.focus();
        if (focus instanceof HTMLInputElement) focus.select();
      });
    }
  }

  function announceChanges(previous: Map<string, ReviewerConnection>): void {
    if (!loaded) return;
    const changed: string[] = [];
    for (const connection of connections.values()) {
      const earlier = previous.get(connection.id);
      if (earlier && earlier.status !== connection.status) {
        changed.push(`${connection.display_label} is now ${STATUS_LABEL[connection.status]}.`);
      }
    }
    if (changed.length) live.textContent = changed.join(" ");
  }

  async function refresh(): Promise<void> {
    if (!sidebarOpen || bridge === null || destroyed) return;
    clearPoll();
    refreshQueued = true;
    if (refreshInFlight !== null) {
      return refreshInFlight;
    }

    const cycle = (async () => {
      while (refreshQueued && sidebarOpen && bridge !== null && !destroyed) {
        refreshQueued = false;
        const activeBridge: ReviewerBridge = bridge;
        const epoch = requestEpoch;
        try {
          const result = await activeBridge.list();
          if (
            destroyed ||
            !sidebarOpen ||
            bridge !== activeBridge ||
            requestEpoch !== epoch
          ) continue;
          const previous = new Map(connections);
          connections.clear();
          for (const connection of result) connections.set(connection.id, connection);
          const revokedWhileConfirming = revokeId !== null &&
            connections.get(revokeId)?.status === "revoked";
          const revokedSetupOwnedFocus = activeSetupId !== null &&
            connections.get(activeSetupId)?.status === "revoked" &&
            root.activeElement !== null &&
            setupPanel.contains(root.activeElement);
          announceChanges(previous);
          loaded = true;
          loadError = null;
          renderLists();
          renderSetup();
          if (revokedWhileConfirming) {
            closeRevoke(false);
            revokedSummary.focus();
          } else if (revokedSetupOwnedFocus) {
            revokedSummary.focus();
          }
        } catch (error) {
          if (
            destroyed ||
            !sidebarOpen ||
            bridge !== activeBridge ||
            requestEpoch !== epoch
          ) continue;
          loadError = friendlyError(error);
          updateChrome();
        }
      }
    })();
    let pending: Promise<void>;
    pending = cycle.then(async () => {
      if (refreshInFlight === pending) refreshInFlight = null;
      if (refreshQueued && sidebarOpen && bridge !== null && !destroyed) {
        return refresh();
      }
      schedulePoll();
    });
    refreshInFlight = pending;
    return pending;
  }

  function selectedClient(): ReviewerClient {
    return (
      clientInputs.find((input) => input.checked)?.value as ReviewerClient | undefined
    ) ?? "chatgpt";
  }

  function renderAddClient(): void {
    const definition = reviewerClient(selectedClient());
    addSetup.textContent = definition.setup;
    addCaveat.textContent = definition.caveat ?? "";
    addCaveat.hidden = definition.caveat === null;
    addSubmit.disabled =
      bridge === null ||
      (addDocumentContext ?? documentContext) === null ||
      definition.availability !== "available";
  }

  function openAddFlow(): void {
    if (bridge === null || documentContext === null) return;
    activeSetupId = null;
    setupPanel.hidden = true;
    addForm.reset();
    const chatgpt = clientInputs.find(({ value }) => value === "chatgpt");
    if (chatgpt) chatgpt.checked = true;
    addLabel.value = reviewerClient("chatgpt").shortName;
    addDocumentContext = documentContext;
    addFlow.hidden = false;
    renderAddClient();
    queueMicrotask(() => {
      addHeading.focus();
      addLabel.select();
    });
  }

  function closeAddFlow(restoreFocus = true): void {
    addFlow.hidden = true;
    addDocumentContext = null;
    if (restoreFocus) addButton.focus();
  }

  function permissionsFrom(
    form: HTMLFormElement,
    context: ReviewerDocumentContext | null = documentContext,
    existing: ReviewerPermissions | null = null,
  ): ReviewerPermissions {
    const choice = form.querySelector<HTMLInputElement>(
      "input[name='reviewer-document-scope']:checked",
    )?.value;
    if (choice !== "current" && choice !== "open" && choice !== "all") {
      throw new Error("Choose document access.");
    }
    const savedDocumentId = choice === "current" &&
        existing?.document_scope === "current" &&
        existing.document_ids.length === 1
      ? existing.document_ids[0]
      : null;
    if (choice !== "all" && savedDocumentId === null && context === null) {
      throw new Error("Wait for the current document to finish opening.");
    }
    const scope: ReviewerDocumentScope = choice === "all" ? "all" : "current";
    const documentId = savedDocumentId ?? context?.id ?? "";
    const checked = (name: string) =>
      form.querySelector<HTMLInputElement>(`input[name='${name}']`)?.checked ?? false;
    return permissionsForScope(scope, documentId, {
      can_read: checked("can_read"),
      can_edit: false,
      can_create: false,
      can_trash: false,
    });
  }

  async function submitAdd(): Promise<void> {
    if (bridge === null) return;
    const label = addLabel.value.trim();
    if (!label) {
      addLabel.setCustomValidity("Enter a reviewer name.");
      addLabel.reportValidity();
      return;
    }
    addLabel.setCustomValidity("");
    const client = selectedClient();
    if (reviewerClient(client).availability !== "available") return;
    const activeBridge = bridge;
    addForm.setAttribute("aria-busy", "true");
    addSubmit.disabled = true;
    requestEpoch += 1;
    try {
      const connection = await activeBridge.create({
        client,
        display_label: label,
        permissions: permissionsFrom(addForm, addDocumentContext),
      });
      if (destroyed || bridge !== activeBridge) return;
      connections.set(connection.id, connection);
      loaded = true;
      loadError = null;
      editing = null;
      closeAddFlow(false);
      renderedSignature = "";
      renderLists(true);
      showSetup(connection.id);
      options.onNotice?.(`${connection.display_label} is ready to set up.`);
    } catch (error) {
      if (destroyed || bridge !== activeBridge) return;
      const message = friendlyError(error);
      loadError = message;
      updateChrome();
      options.onNotice?.(`Could not add reviewer: ${message}`, "error");
    } finally {
      addForm.removeAttribute("aria-busy");
      renderAddClient();
      if (sidebarOpen) {
        refreshQueued = true;
        if (refreshInFlight === null) void refresh();
      }
    }
  }

  function commandFor(connection: ReviewerConnection): string | null {
    return reviewerSetupCommand(connection.client, stdioExecutable, connection.id);
  }

  function renderSetup(): void {
    const connection = activeSetupId ? connections.get(activeSetupId) : null;
    if (!connection || connection.status === "revoked") {
      setupPanel.hidden = true;
      activeSetupId = null;
      resetConfirmationOpen = false;
      resetExpectedRevision = null;
      return;
    }
    const definition = reviewerClient(connection.client);
    const command = commandFor(connection);
    setupPanel.hidden = false;
    setupHeading.textContent = `${connection.display_label} setup`;
    setupStatus.textContent = STATUS_LABEL[connection.status];
    setupStatus.dataset.status = connection.status;
    setupInstructions.textContent = definition.setup;
    setupCaveat.textContent = definition.caveat ?? "";
    setupCaveat.hidden = definition.caveat === null;
    setupCommand.textContent = command ??
      (definition.availability === "planned"
        ? "Claude Desktop setup is not available yet."
        : "Waiting for the packaged local connector…");
    setupCommand.dataset.placeholder = String(command === null);
    setupCopy.disabled = command === null;
    setupCopy.textContent = command ? "Copy setup" : "Setup unavailable";
    resetWarning.hidden = !resetConfirmationOpen;
    setupReset.hidden = resetConfirmationOpen;
  }

  function showSetup(id: string, confirmReset = false): void {
    const connection = connections.get(id);
    if (!connection) return;
    activeSetupId = id;
    resetConfirmationOpen = confirmReset;
    resetExpectedRevision = confirmReset ? connection.revision : null;
    addFlow.hidden = true;
    renderSetup();
    queueMicrotask(() => (confirmReset ? resetCancel : setupHeading).focus());
  }

  function closeSetup(): void {
    const id = activeSetupId;
    activeSetupId = null;
    resetConfirmationOpen = false;
    resetExpectedRevision = null;
    setupPanel.hidden = true;
    const action = findAction(id, "setup") ?? addButton;
    action.focus();
  }

  function findAction(id: string | null, action: string): HTMLButtonElement | null {
    if (!id) return null;
    return [...list.querySelectorAll<HTMLButtonElement>("button[data-reviewer-action]")].find(
      (candidate) =>
        candidate.dataset.reviewerId === id && candidate.dataset.reviewerAction === action,
    ) ?? null;
  }

  async function resetConnection(): Promise<void> {
    const connection = activeSetupId ? connections.get(activeSetupId) : null;
    if (!connection || bridge === null || resetExpectedRevision === null) return;
    const activeBridge = bridge;
    resetConfirm.disabled = true;
    requestEpoch += 1;
    try {
      const updated = await activeBridge.reset(connection.id, resetExpectedRevision);
      if (destroyed || bridge !== activeBridge) return;
      connections.set(updated.id, updated);
      renderedSignature = "";
      renderLists(true);
      resetConfirmationOpen = false;
      resetExpectedRevision = null;
      renderSetup();
      (setupCopy.disabled ? setupDone : setupCopy).focus();
      live.textContent = `${updated.display_label} connection was reset. Existing sessions no longer have access.`;
    } catch (error) {
      if (destroyed || bridge !== activeBridge) return;
      const message = friendlyError(error);
      options.onNotice?.(`Could not reset connection: ${message}`, "error");
      loadError = message;
      updateChrome();
    } finally {
      resetConfirm.disabled = false;
      if (sidebarOpen) {
        refreshQueued = true;
        if (refreshInFlight === null) void refresh();
      }
    }
  }

  function startEdit(id: string, kind: EditKind): void {
    const connection = connections.get(id);
    if (!connection) return;
    activeSetupId = null;
    setupPanel.hidden = true;
    editing = {
      id,
      kind,
      expectedRevision: connection.revision,
      documentContext,
    };
    renderedSignature = "";
    renderLists(true);
  }

  function cancelEdit(id: string): void {
    const kind = editing?.id === id ? editing.kind : null;
    editing = null;
    renderedSignature = "";
    renderLists(true);
    findAction(id, kind ?? "rename")?.focus();
  }

  async function submitEdit(form: HTMLFormElement): Promise<void> {
    const id = form.dataset.reviewerId;
    const kind = form.dataset.reviewerForm as EditKind | undefined;
    const connection = id ? connections.get(id) : null;
    if (!connection || bridge === null || (kind !== "rename" && kind !== "permissions")) return;
    const activeBridge = bridge;
    const submit = form.querySelector<HTMLButtonElement>("button[type='submit']");
    if (submit) submit.disabled = true;
    form.setAttribute("aria-busy", "true");
    requestEpoch += 1;
    try {
      const expectedRevision = Number(form.dataset.expectedRevision);
      if (!Number.isSafeInteger(expectedRevision) || expectedRevision < 0) {
        throw new Error("Reviewer state changed. Close this editor and try again.");
      }
      const input = kind === "rename"
        ? {
            expected_revision: expectedRevision,
            display_label:
              form.querySelector<HTMLInputElement>("input[name='display_label']")?.value.trim() ?? "",
          }
        : {
            expected_revision: expectedRevision,
            permissions: permissionsFrom(
              form,
              editing?.documentContext ?? documentContext,
              connection.permissions,
            ),
          };
      if ("display_label" in input && !input.display_label) {
        throw new Error("Enter a reviewer name.");
      }
      const updated = await activeBridge.update(connection.id, input);
      if (destroyed || bridge !== activeBridge) return;
      connections.set(updated.id, updated);
      editing = null;
      renderedSignature = "";
      renderLists(true);
      findAction(updated.id, kind)?.focus();
      live.textContent = kind === "rename"
        ? `Reviewer renamed to ${updated.display_label}.`
        : `${updated.display_label} access updated.`;
    } catch (error) {
      if (destroyed || bridge !== activeBridge) return;
      const message = friendlyError(error);
      options.onNotice?.(`Could not update reviewer: ${message}`, "error");
      loadError = message;
      updateChrome();
      if (submit) submit.disabled = false;
      form.removeAttribute("aria-busy");
    } finally {
      if (sidebarOpen) {
        refreshQueued = true;
        if (refreshInFlight === null) void refresh();
      }
    }
  }

  function setModalBlocked(blocked: boolean): void {
    if (blocked) {
      for (const element of root.body.children) {
        if (element === revokeBackdrop || !(element instanceof HTMLElement)) continue;
        if (!element.hasAttribute("inert")) {
          element.setAttribute("inert", "");
          modalBlockedElements.add(element);
        }
      }
      return;
    }
    for (const element of modalBlockedElements) element.removeAttribute("inert");
    modalBlockedElements.clear();
  }

  function openRevoke(id: string, returnFocus: HTMLElement): void {
    const connection = connections.get(id);
    if (!connection || connection.status === "revoked") return;
    revokeId = id;
    revokeExpectedRevision = connection.revision;
    revokeReturnFocus = returnFocus;
    revokeName.textContent = connection.display_label;
    revokeError.textContent = "";
    revokeError.hidden = true;
    revokeBackdrop.hidden = false;
    setModalBlocked(true);
    queueMicrotask(() => revokeCancel.focus());
  }

  function closeRevoke(restoreFocus = true): void {
    revokeId = null;
    revokeExpectedRevision = null;
    revokeBackdrop.hidden = true;
    setModalBlocked(false);
    if (restoreFocus) revokeReturnFocus?.focus();
    revokeReturnFocus = null;
  }

  async function confirmRevoke(): Promise<void> {
    const connection = revokeId ? connections.get(revokeId) : null;
    if (!connection || bridge === null || revokeExpectedRevision === null) return;
    const activeBridge = bridge;
    revokeConfirm.disabled = true;
    requestEpoch += 1;
    try {
      const revoked = await activeBridge.revoke(connection.id, revokeExpectedRevision);
      if (destroyed || bridge !== activeBridge) return;
      connections.set(revoked.id, revoked);
      editing = null;
      if (activeSetupId === revoked.id) closeSetup();
      closeRevoke(false);
      renderedSignature = "";
      renderLists(true);
      live.textContent = `${revoked.display_label} access removed.`;
      revokedSummary.focus();
    } catch (error) {
      if (destroyed || bridge !== activeBridge) return;
      const message = friendlyError(error);
      options.onNotice?.(`Could not remove access: ${message}`, "error");
      loadError = message;
      updateChrome();
      revokeError.textContent = `Could not remove access: ${message}`;
      revokeError.hidden = false;
      revokeError.focus();
    } finally {
      revokeConfirm.disabled = false;
      if (sidebarOpen) {
        refreshQueued = true;
        if (refreshInFlight === null) void refresh();
      }
    }
  }

  listen(addButton, "click", openAddFlow);
  listen(addCancel, "click", () => closeAddFlow());
  listen(retry, "click", () => {
    loadError = null;
    updateChrome();
    void refresh();
  });
  listen(addForm, "submit", (event) => {
    event.preventDefault();
    void submitAdd();
  });
  listen(addForm, "change", (event) => {
    const input = event.target;
    if (!(input instanceof HTMLInputElement)) return;
    if (input.name === "reviewer-document-scope") {
      return;
    }
    if (input.name !== "reviewer-client") return;
    const previousDefaults = REVIEWER_CLIENTS.map(({ shortName }) => shortName);
    if (!addLabel.value.trim() || previousDefaults.includes(addLabel.value.trim())) {
      addLabel.value = reviewerClient(selectedClient()).shortName;
    }
    renderAddClient();
  });
  listen(addLabel, "input", () => addLabel.setCustomValidity(""));
  listen(list, "click", (event) => {
    const target = event.target instanceof Element
      ? event.target.closest<HTMLButtonElement>("button[data-reviewer-action]")
      : null;
    const id = target?.dataset.reviewerId;
    const action = target?.dataset.reviewerAction;
    if (!target || !id || !action) return;
    if (action === "setup") showSetup(id);
    else if (action === "reset") showSetup(id, true);
    else if (action === "rename" || action === "permissions") startEdit(id, action);
    else if (action === "cancel-edit") cancelEdit(id);
    else if (action === "revoke") openRevoke(id, target);
  });
  listen(list, "submit", (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || !form.dataset.reviewerForm) return;
    event.preventDefault();
    void submitEdit(form);
  });
  listen(setupCopy, "click", () => {
    const connection = activeSetupId ? connections.get(activeSetupId) : null;
    const command = connection ? commandFor(connection) : null;
    if (!command) return;
    void copyText(command)
      .then(() => {
        setupCopy.textContent = "Copied";
        window.setTimeout(renderSetup, 1_200);
      })
      .catch(() => options.onNotice?.("Could not copy the setup command.", "error"));
  });
  listen(setupReset, "click", () => {
    const connection = activeSetupId ? connections.get(activeSetupId) : null;
    if (!connection) return;
    resetConfirmationOpen = true;
    resetExpectedRevision = connection.revision;
    renderSetup();
    resetCancel.focus();
  });
  listen(resetCancel, "click", () => {
    resetConfirmationOpen = false;
    resetExpectedRevision = null;
    renderSetup();
    setupReset.focus();
  });
  listen(resetConfirm, "click", () => void resetConnection());
  listen(setupDone, "click", closeSetup);
  listen(revokeCancel, "click", () => closeRevoke());
  listen(revokeConfirm, "click", () => void confirmRevoke());
  listen(revokeBackdrop, "mousedown", (event) => {
    if (event.target === revokeBackdrop) closeRevoke();
  });

  const modalKeydown = (event: KeyboardEvent) => {
    if (revokeBackdrop.hidden) return;
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopImmediatePropagation();
      closeRevoke();
      return;
    }
    if (event.key !== "Tab") return;
    const controls = [revokeCancel, revokeConfirm].filter(({ disabled }) => !disabled);
    const current = controls.indexOf(root.activeElement as HTMLButtonElement);
    const next = event.shiftKey
      ? (current <= 0 ? controls.length : current) - 1
      : (current + 1) % controls.length;
    event.preventDefault();
    event.stopImmediatePropagation();
    controls[next].focus();
  };
  root.addEventListener("keydown", modalKeydown, true);
  disposers.push(() => root.removeEventListener("keydown", modalKeydown, true));

  renderAddClient();
  renderLists(true);

  return {
    isModalOpen: () => !revokeBackdrop.hidden,
    dismissModal() {
      if (revokeBackdrop.hidden) return false;
      closeRevoke();
      return true;
    },
    setBridge(nextBridge) {
      if (bridge === nextBridge) return;
      if (!revokeBackdrop.hidden) closeRevoke(false);
      bridge = nextBridge;
      requestEpoch += 1;
      refreshQueued = false;
      clearPoll();
      connections.clear();
      loaded = false;
      loadError = null;
      editing = null;
      activeSetupId = null;
      resetConfirmationOpen = false;
      resetExpectedRevision = null;
      setupPanel.hidden = true;
      renderedSignature = "";
      renderLists(true);
      if (sidebarOpen && bridge !== null) void refresh();
    },
    setSidebarOpen(open) {
      if (sidebarOpen === open) return;
      sidebarOpen = open;
      if (!open) {
        requestEpoch += 1;
        refreshQueued = false;
        clearPoll();
        if (!revokeBackdrop.hidden) closeRevoke(false);
        return;
      }
      void refresh();
    },
    setDocumentContext(context) {
      if (
        documentContext?.id === context?.id &&
        documentContext?.title === context?.title
      ) return;
      documentContext = context;
      updateChrome();
      renderedSignature = "";
      renderLists();
      renderAddClient();
    },
    setStdioExecutable(path) {
      stdioExecutable = path.trim();
      renderSetup();
    },
    refresh,
    destroy() {
      destroyed = true;
      requestEpoch += 1;
      clearPoll();
      closeRevoke(false);
      for (const dispose of disposers.splice(0)) dispose();
      connections.clear();
    },
  };
}
