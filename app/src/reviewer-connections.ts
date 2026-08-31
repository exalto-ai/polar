import {
  reviewerClientName,
  reviewerSetupCommand,
  reviewerSetupCopyLabel,
  reviewerSetupInstructions,
  reviewerSetupServerName,
  type ReviewerClient,
} from "./reviewer-setup";
import { writeClipboardText } from "./clipboard";

export type ReviewerAccess = {
  document_scope: "current" | "all";
  document_id: string | null;
};

export type ReviewerConnection = {
  id: string;
  client: ReviewerClient;
  provider: "openai" | "anthropic";
  display_label: string;
  status: "configured" | "revoked";
  access: ReviewerAccess;
  revision: number;
  created_at: number;
  last_seen_at: number | null;
  revoked_at: number | null;
  reported_model: string | null;
};

export type ReviewerInput = {
  client: ReviewerClient;
  display_label: string;
  access: ReviewerAccess;
};

export type ReviewerApi = {
  listReviewerConnections(): Promise<ReviewerConnection[]>;
  createReviewerConnection(input: ReviewerInput): Promise<ReviewerConnection>;
  updateReviewerConnection(
    id: string,
    input: Omit<ReviewerInput, "client"> & { expected_revision: number },
  ): Promise<ReviewerConnection>;
  resetReviewerConnection(id: string, expectedRevision: number): Promise<ReviewerConnection>;
  revokeReviewerConnection(id: string, expectedRevision: number): Promise<ReviewerConnection>;
};

export type ReviewerDocumentContext = { id: string; title: string };

type Options = {
  api?: ReviewerApi | null;
  copyText?: (text: string) => Promise<void>;
  confirmAction?: (message: string) => boolean;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type ReviewerController = {
  setApi(api: ReviewerApi | null): void;
  setDocument(context: ReviewerDocumentContext | null): void;
  setExecutable(path: string): void;
  setOpen(open: boolean): void;
  destroy(): void;
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing reviewer element: ${selector}`);
  return value;
}

export function installReviewerConnections(
  root: Document,
  options: Options = {},
): ReviewerController {
  const add = required<HTMLButtonElement>(root, "#reviewer-add");
  const checkStatus = required<HTMLButtonElement>(root, "#reviewer-refresh");
  const list = required<HTMLUListElement>(root, "#reviewer-list");
  const empty = required<HTMLElement>(root, "#reviewer-empty");
  const error = required<HTMLElement>(root, "#reviewer-error");
  const form = required<HTMLFormElement>(root, "#reviewer-form");
  const formTitle = required<HTMLElement>(root, "#reviewer-form-title");
  const client = required<HTMLSelectElement>(root, "#reviewer-client");
  const label = required<HTMLInputElement>(root, "#reviewer-label");
  const scope = required<HTMLSelectElement>(root, "#reviewer-scope");
  const current = required<HTMLElement>(root, "#reviewer-current");
  const cancel = required<HTMLButtonElement>(root, "#reviewer-cancel");
  const setup = required<HTMLElement>(root, "#reviewer-setup");
  const setupText = required<HTMLElement>(root, "#reviewer-setup-text");
  const setupName = required<HTMLElement>(root, "#reviewer-setup-name");
  const setupCommand = required<HTMLElement>(root, "#reviewer-setup-command");
  const copy = required<HTMLButtonElement>(root, "#reviewer-copy");
  const setupDone = required<HTMLButtonElement>(root, "#reviewer-setup-done");
  const copyText = options.copyText ?? writeClipboardText;
  const confirmAction = options.confirmAction ?? ((message: string) => window.confirm(message));
  const disposers: Array<() => void> = [];
  let api = options.api ?? null;
  let documentContext: ReviewerDocumentContext | null = null;
  let executable = "";
  let connections: ReviewerConnection[] = [];
  let editing: ReviewerConnection | null = null;
  let open = false;
  let generation = 0;

  function listen(target: EventTarget, event: string, listener: EventListener) {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function showError(value: unknown) {
    error.textContent = value instanceof Error ? value.message : String(value);
    error.hidden = false;
  }

  function accessFromForm(): ReviewerAccess {
    return scope.value === "all"
      ? { document_scope: "all", document_id: null }
      : { document_scope: "current", document_id: documentContext?.id ?? null };
  }

  function renderDocument() {
    current.textContent = documentContext
      ? `Current: ${documentContext.title || "Untitled"}`
      : "Open a document to use current-document access.";
    scope.querySelector<HTMLOptionElement>('option[value="current"]')!.disabled =
      !documentContext;
    if (!documentContext && scope.value === "current") scope.value = "all";
  }

  function openForm(connection: ReviewerConnection | null) {
    editing = connection;
    formTitle.textContent = connection ? "Edit reviewer" : "Add reviewer";
    client.disabled = Boolean(connection);
    client.value = connection?.client ?? "chatgpt";
    label.value = connection?.display_label ?? "";
    scope.value = connection?.access.document_scope ?? (documentContext ? "current" : "all");
    renderDocument();
    setup.hidden = true;
    form.hidden = false;
    queueMicrotask(() => label.focus());
  }

  function showSetup(connection: ReviewerConnection) {
    const command = reviewerSetupCommand(connection.client, executable, connection.id);
    setupText.textContent = reviewerSetupInstructions(connection.client);
    setupName.textContent = `Server name: ${reviewerSetupServerName(connection.id) ?? "Unavailable"}`;
    setupCommand.textContent = command ?? "Setup is unavailable in this build.";
    copy.textContent = reviewerSetupCopyLabel(connection.client);
    copy.disabled = !command;
    form.hidden = true;
    setup.hidden = false;
  }

  function button(text: string, action: () => void): HTMLButtonElement {
    const value = document.createElement("button");
    value.type = "button";
    value.textContent = text;
    value.addEventListener("click", action);
    return value;
  }

  function render() {
    const active = connections.filter((connection) => connection.status === "configured");
    empty.hidden = active.length > 0;
    list.replaceChildren(
      ...active.map((connection) => {
        const item = document.createElement("li");
        const details = document.createElement("div");
        const title = document.createElement("strong");
        const meta = document.createElement("small");
        const activity = document.createElement("small");
        const actions = document.createElement("div");
        title.textContent = connection.display_label;
        meta.textContent = `${reviewerClientName(connection.client)} · ${connection.access.document_scope === "all" ? "All documents" : "Current document"}`;
        activity.textContent = reviewerActivity(connection);
        details.append(title, meta, activity);
        actions.className = "reviewer-actions";
        actions.append(
          button("Setup", () => showSetup(connection)),
          button("Edit", () => openForm(connection)),
          button("Reset", () => void reset(connection)),
          button("Remove", () => void revoke(connection)),
        );
        item.append(details, actions);
        return item;
      }),
    );
  }

  async function refresh() {
    if (!api) return;
    const request = ++generation;
    try {
      const values = await api.listReviewerConnections();
      if (request !== generation) return;
      connections = values;
      error.hidden = true;
      render();
    } catch (reason) {
      if (request === generation) showError(reason);
    }
  }

  async function reset(connection: ReviewerConnection) {
    if (!api || !confirmAction(`Reset ${connection.display_label}? Existing sessions will stop.`)) return;
    generation += 1;
    try {
      const updated = await api.resetReviewerConnection(connection.id, connection.revision);
      connections = connections.map((value) => (value.id === updated.id ? updated : value));
      render();
      showSetup(updated);
    } catch (reason) {
      showError(reason);
    }
  }

  async function revoke(connection: ReviewerConnection) {
    if (!api || !confirmAction(`Remove access for ${connection.display_label}?`)) return;
    generation += 1;
    try {
      const updated = await api.revokeReviewerConnection(connection.id, connection.revision);
      connections = connections.map((value) => (value.id === updated.id ? updated : value));
      render();
    } catch (reason) {
      showError(reason);
    }
  }

  listen(add, "click", () => openForm(null));
  listen(checkStatus, "click", () => void refresh());
  listen(cancel, "click", () => {
    form.hidden = true;
    editing = null;
  });
  listen(setupDone, "click", () => (setup.hidden = true));
  listen(copy, "click", () => {
    if (!setupCommand.textContent || copy.disabled) return;
    void copyText(setupCommand.textContent)
      .then(() => options.onNotice?.("Setup copied."))
      .catch(() => options.onNotice?.("Could not copy setup.", "error"));
  });
  listen(form, "submit", (event) => {
    event.preventDefault();
    if (!api) return;
    generation += 1;
    const input = {
      display_label: label.value,
      access: accessFromForm(),
    };
    void (editing
      ? api.updateReviewerConnection(editing.id, {
          ...input,
          expected_revision: editing.revision,
        })
      : api.createReviewerConnection({
          ...input,
          client: client.value as ReviewerClient,
        }))
      .then((saved) => {
        const exists = connections.some((value) => value.id === saved.id);
        connections = exists
          ? connections.map((value) => (value.id === saved.id ? saved : value))
          : [saved, ...connections];
        editing = null;
        render();
        if (exists) form.hidden = true;
        else showSetup(saved);
      })
      .catch(showError);
  });

  renderDocument();
  render();

  return {
    setApi(value) {
      api = value;
      if (open) void refresh();
    },
    setDocument(value) {
      documentContext = value;
      renderDocument();
    },
    setExecutable(value) {
      executable = value;
    },
    setOpen(value) {
      open = value;
      if (open) void refresh();
    },
    destroy() {
      generation += 1;
      for (const dispose of disposers.splice(0)) dispose();
    },
  };
}

export function reviewerActivity(
  connection: Pick<ReviewerConnection, "last_seen_at" | "reported_model">,
  now = Date.now(),
): string {
  const model = connection.reported_model?.trim();
  const modelText = model ? ` · ${model} (reported)` : "";
  if (connection.last_seen_at === null) return `Not used yet${modelText}`;
  const elapsed = Math.max(0, now - connection.last_seen_at);
  if (elapsed < 60_000) return `Last used just now${modelText}`;
  if (elapsed < 3_600_000) {
    const minutes = Math.floor(elapsed / 60_000);
    return `Last used ${minutes} min ago${modelText}`;
  }
  const date = new Date(connection.last_seen_at).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
  return `Last used ${date}${modelText}`;
}
