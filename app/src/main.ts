/**
 * The window. Opens one document at a time; the switcher opens with the
 * platform accelerator and K.
 */
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow as tauriWindow } from "@tauri-apps/api/window";
import * as Y from "yjs";
import { Awareness } from "y-protocols/awareness";
import type { Editor } from "@tiptap/core";
import { installAiSupport } from "./ai-support";
import { tauriProChatBridge } from "./pro-chat-bridge";
import { tauriProProviderBridge } from "./pro-provider-bridge";
import { createEditor } from "./editor";
import { EditorApi } from "./editor-api";
import { installCurrentSources } from "./current-sources";
import { reviewerBridge } from "./reviewer-bridge";
import {
  exportMarkdownDocument,
  importMarkdownDocument,
  nativeFileBridge,
} from "./files";
import { ACCEL_LABEL, accel, relabelShortcutHints } from "./keys";
import { Mcp, type DocumentSummary } from "./mcp";
import { colorFor, playfulName, seedFrom } from "./names";
import {
  installProvenanceRails,
  toolReportedModelLabel,
  type Rails,
} from "./provenance";
import { SyncProvider, type AgentPresence, type ProviderStatus } from "./provider";
import {
  installSuggestionReview,
  suggestionPositionAtSelection,
  type SuggestionReviewController,
} from "./suggestions";

type Connection = {
  sync_url: string;
  mcp_url: string;
  token: string;
  stdio_command: string;
  /** Who this window writes as, from `thought_mcp::EDITOR_ACTOR_ID`. */
  actor_id: string;
};

const els = {
  status: document.getElementById("status")!,
  presence: document.getElementById("presence")!,
  editor: document.getElementById("editor")!,
  scrim: document.getElementById("scrim")!,
  connections: document.getElementById("connections")!,
  peers: document.getElementById("peers")!,
  agents: document.getElementById("agents")!,
  toast: document.getElementById("toast")!,
  switcher: document.querySelector(".switcher") as HTMLElement,
  hint: document.getElementById("switcher-hint")!,
  input: document.getElementById("switcher-input") as HTMLInputElement,
  results: document.getElementById("switcher-results")!,
};

let connection: Connection;
let mcp: Mcp;
let editorApi: EditorApi;
let open: {
  doc: Y.Doc;
  awareness: Awareness;
  provider: SyncProvider;
  editor: Editor;
  rails: Rails;
  suggestions: SuggestionReviewController;
  stopChatHydration: () => void;
} | null = null;
let openDocId = "";
let closingAfterAutosave = false;

/**
 * Say something went wrong, where the person is already looking.
 *
 * Failures used to have nowhere to go. `refreshProvenance` swallowed its
 * errors, `boot` only wrote into the title, and everything else surfaced as
 * nothing at all — a green status dot above a window that was quietly wrong.
 */
let toastTimer: number | null = null;

function notify(message: string, kind: "info" | "error" = "info") {
  els.toast.textContent = message;
  els.toast.dataset.kind = kind;
  els.toast.hidden = false;
  if (toastTimer !== null) clearTimeout(toastTimer);
  // Errors linger; confirmations do not.
  toastTimer = window.setTimeout(
    () => (els.toast.hidden = true),
    kind === "error" ? 6000 : 2600,
  );
}

/** Whatever an unknown throw carries, said in one line. */
function reason(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error);
  return text.length > 160 ? `${text.slice(0, 157)}…` : text;
}

const aiSupport = installAiSupport(document, {
  providerBridge: tauriProProviderBridge(),
  chatBridge: tauriProChatBridge(),
  onNotice: notify,
  onChatResponseCopied: () => open?.editor.commands.focus(),
  onChatSuggestionCreated: () => {
    void open?.suggestions.refresh();
    open?.editor.commands.focus();
  },
});

async function visibleWordingRevision(): Promise<string | null> {
  const current = open;
  const docId = openDocId;
  if (
    !current ||
    !docId ||
    !isTauri() ||
    !current.provider.isHydrated ||
    current.provider.hasPendingChanges
  ) {
    return null;
  }
  const revision = await invoke<string>("document_wording_revision", {
    document: current.editor.getJSON(),
  });
  if (
    open !== current ||
    openDocId !== docId ||
    !current.provider.isHydrated ||
    current.provider.hasPendingChanges
  ) {
    return null;
  }
  return revision;
}

const currentSources = installCurrentSources(
  document,
  (docId) => mcp.documentLineage(docId),
  visibleWordingRevision,
);

/**
 * Agents that have written recently.
 *
 * An agent has no session to be "in" — it connects over MCP, does something,
 * and leaves. So presence here means *recently active*, and it lapses on its
 * own rather than waiting for a disconnect that never comes.
 */
const AGENT_PRESENCE_MS = 45_000;
const activeAgents = new Map<string, { presence: AgentPresence; at: number }>();

function noteAgent(presence: AgentPresence) {
  activeAgents.set(presence.actor_id, { presence, at: Date.now() });
  renderPeers();
  if (!els.connections.hidden) void refreshConnectionActors();
  scheduleProvenance();
  // Re-render when this one lapses, so the chip disappears without a further
  // edit to trigger it.
  setTimeout(renderPeers, AGENT_PRESENCE_MS + 250);
}

function liveAgents() {
  const cutoff = Date.now() - AGENT_PRESENCE_MS;
  for (const [id, entry] of activeAgents) {
    if (entry.at < cutoff) activeAgents.delete(id);
  }
  return [...activeAgents.values()];
}

function setStatus(status: ProviderStatus) {
  els.status.dataset.state = status;
  els.status.title = status;
}

/**
 * The document title, shown in the native window title rather than painted
 * into the page — a heading repeated two centimetres above itself is noise.
 *
 * Derived exactly as the daemon derives it (first heading, else first non-empty
 * block), because two implementations of "what is this document called" drift
 * and then disagree in front of the user.
 */
function deriveTitle(editor: Editor): string {
  const doc = editor.state.doc;
  let title = "";
  doc.forEach((node) => {
    if (title) return;
    if (node.type.name === "heading") title = node.textContent.trim();
  });
  if (!title) {
    doc.forEach((node) => {
      if (!title && node.textContent.trim()) title = node.textContent.trim();
    });
  }
  return title.slice(0, 120) || "Untitled";
}

function refreshTitle(editor: Editor) {
  const title = deriveTitle(editor);
  document.title = title;
  void getCurrentWindow?.()?.setTitle(title);
  if (open?.editor === editor && openDocId) {
    if (!open.provider.isHydrated) {
      aiSupport.setCurrentDocument(null);
      return;
    }
    aiSupport.setCurrentDocument({
      id: openDocId,
      title,
      snapshot: () => {
        if (open?.editor !== editor) throw new Error("This document is no longer open.");
        if (!open.provider.isHydrated) throw new Error("This document is still opening.");
        return editor.getJSON();
      },
      suggestionPosition: () => {
        if (open?.editor !== editor) throw new Error("This document is no longer open.");
        return suggestionPositionAtSelection(editor, open.doc);
      },
      waitUntilSaved: () => {
        if (open?.editor !== editor) return Promise.resolve(false);
        return open.provider.waitUntilSaved();
      },
    });
  }
}

/** Show or hide one peer's caret label from outside the editor. */
function pointAt(peerId: number, pointed: boolean) {
  document
    .querySelectorAll(`.peer-caret[data-peer="${peerId}"]`)
    .forEach((caret) => caret.classList.toggle("is-pointed", pointed));
}

function renderPeers() {
  if (!open) return;
  renderPresence(open.awareness, open.doc.clientID);
  if (!els.connections.hidden) renderConnectionPeers();
}

/**
 * Remember which document this window had open.
 *
 * Session storage is per window, so two windows no longer overwrite each
 * other's idea of "the last document" — which they did, and meant opening a
 * second window could yank the first one somewhere else on the next launch.
 * The shared copy survives as the starting point for a brand-new window.
 */
function rememberOpenDocument(docId: string) {
  window.sessionStorage.setItem("thought.last", docId);
  window.localStorage.setItem("thought.last", docId);
}

function lastOpenDocument(): string | null {
  return (
    window.sessionStorage.getItem("thought.last") ??
    window.localStorage.getItem("thought.last")
  );
}

function initials(name: string): string {
  return name
    .split(/[\s:_-]+/)
    .map((word) => word[0])
    .join("")
    .slice(0, 2)
    .toUpperCase();
}

function renderPresence(awareness: Awareness, self: number) {
  const others = [...awareness.getStates().entries()].filter(([id]) => id !== self);
  const windows = others.map(([id, state]) => {
    const user = (state as { user?: { name: string; color: string } }).user;
    const name = user?.name ?? "Someone";
    const chip = document.createElement("span");
    chip.className = "who";
    chip.style.setProperty("--who", user?.color ?? "#888");
    // Initials of both words, so two peers are told apart at a glance.
    chip.textContent = initials(name);
    chip.title = `${name} — click to find their cursor`;

    // Pointing at a chip points at that peer's caret, which is otherwise a
    // 2px line somewhere in the document.
    chip.addEventListener("mouseenter", () => pointAt(id, true));
    chip.addEventListener("mouseleave", () => pointAt(id, false));
    chip.addEventListener("click", () => {
      const caret = document.querySelector(`.peer-caret[data-peer="${id}"]`);
      caret?.scrollIntoView({ behavior: "smooth", block: "center" });
      pointAt(id, true);
      setTimeout(() => pointAt(id, false), 1800);
    });
    return chip;
  });

  // Agents are shown differently on purpose: a window is *present*, an agent
  // has *just written*. Marking them the same would claim a kind of liveness
  // agents do not have.
  const agents = liveAgents().map(({ presence, at }) => {
    const chip = document.createElement("span");
    chip.className = "who is-agent";
    chip.style.setProperty("--who", colorFor(seedFrom(presence.actor_id)));
    chip.textContent = initials(presence.name || presence.actor_id);
    const model = presence.model ? ` · ${toolReportedModelLabel(presence.model)}` : "";
    chip.title = `${presence.name}${model} — wrote ${ago(at)}, hover to see where`;

    // Pointing at an agent's chip lights the blocks it wrote, the same bargain
    // the window chips make with carets. Deliberately not offered for window
    // chips: every window on this device writes as one actor, so lighting them
    // would highlight the user's own blocks as if a peer had written them.
    chip.addEventListener("mouseenter", () => open?.rails.highlight(presence.actor_id));
    chip.addEventListener("mouseleave", () => open?.rails.highlight(null));
    return chip;
  });

  els.presence.replaceChildren(...agents, ...windows);
}

async function canLeaveCurrentDocument(): Promise<boolean> {
  if (!open?.provider.hasPendingChanges) return true;
  if (await open.provider.waitUntilSaved()) return true;
  notify(
    "This document still has changes waiting to autosave. Reconnect before switching.",
    "error",
  );
  return false;
}

async function openDocument(docId: string): Promise<boolean> {
  if (open && openDocId === docId) {
    open.editor.commands.focus();
    return true;
  }
  if (!(await canLeaveCurrentDocument())) return false;
  aiSupport.setCurrentDocument(null);
  currentSources.setDocument(null);
  open?.suggestions.destroy();
  open?.rails.destroy();
  open?.stopChatHydration();
  open?.provider.destroy();
  open?.editor.destroy();
  els.editor.replaceChildren();

  const doc = new Y.Doc();
  const awareness = new Awareness(doc);
  const provider = new SyncProvider(
    connection.sync_url,
    connection.token,
    docId,
    doc,
    awareness,
    setStatus,
    noteAgent,
  );

  // A window has no name of its own, and "Window 72" tells you nothing. Both
  // name and colour derive from the Yjs client id, so a peer keeps the same
  // identity for as long as it is connected.
  const user = {
    name: playfulName(doc.clientID),
    color: colorFor(doc.clientID),
    id: doc.clientID,
  };
  const editor = createEditor(
    document.body,
    els.editor,
    doc,
    awareness,
    provider,
    user,
    {
      newDocument: createNewDocument,
      importMarkdown: importMarkdownFile,
      exportMarkdown: async () => {
        await exportMarkdownFile();
      },
    },
    () => !aiSupport.isOpen(),
  );
  awareness.setLocalStateField("user", user);

  const rails = installProvenanceRails(editor, doc, els.editor, connection.actor_id);
  const suggestions = installSuggestionReview(editor, doc, docId, editorApi, {
    beforeDecision: () => provider.waitUntilSaved(),
    onNotice: notify,
  });

  provider.connect();
  open = {
    doc,
    awareness,
    provider,
    editor,
    rails,
    suggestions,
    stopChatHydration: () => {},
  };
  openDocId = docId;
  currentSources.setDocument(docId);
  const opened = open;
  opened.stopChatHydration = provider.subscribeHydration((hydrated) => {
    if (open !== opened) return;
    if (!hydrated) {
      aiSupport.setCurrentDocument(null);
      return;
    }
    refreshTitle(editor);
  });

  // Exposed in development so the editor can be driven directly. Synthetic
  // key events do not reach ProseMirror's input handling reliably, which makes
  // anything keyboard-driven hard to check any other way.
  if (import.meta.env?.DEV) {
    (window as unknown as { __thought?: unknown }).__thought = { editor, doc, provider };
  }

  editor.on("update", () => refreshTitle(editor));
  editor.on("update", scheduleProvenance);
  editor.on("update", currentSources.scheduleRefresh);
  const stopSourceHydration = provider.subscribeHydration((hydrated) => {
    if (hydrated) currentSources.scheduleRefresh();
  });
  const stopSourceSaveStatus = provider.subscribeSaveStatus((status) => {
    if (status === "saved") currentSources.scheduleRefresh();
  });
  editor.on("destroy", stopSourceHydration);
  editor.on("destroy", stopSourceSaveStatus);
  awareness.on("change", renderPeers);
  refreshTitle(editor);
  activeAgents.clear();
  renderPeers();
  rememberOpenDocument(docId);
  void refreshProvenance();
  return true;
}

/**
 * Re-ask the daemon who wrote what.
 *
 * Attribution lives in the op log, not the CRDT (AD-1), so it does not ride the
 * update frames — it is fetched. Debounced because a burst of keystrokes is one
 * question, not forty, and the answer only moves once the daemon has committed.
 */
const PROVENANCE_DEBOUNCE_MS = 400;
let provenanceTimer: number | null = null;

async function refreshProvenance() {
  if (!open) return;
  const docId = openDocId;
  try {
    const blocks = await mcp.blockProvenance(docId);
    // The switcher may have moved on while this was in flight.
    if (open && openDocId === docId) open.rails.setProvenance(blocks);
  } catch (error) {
    // The rails keep saying whatever they last knew, but silence here is how a
    // stale margin looks exactly like an accurate one.
    notify(`Could not load authorship: ${reason(error)}`, "error");
  }
}

function scheduleProvenance() {
  if (provenanceTimer !== null) clearTimeout(provenanceTimer);
  provenanceTimer = window.setTimeout(() => void refreshProvenance(), PROVENANCE_DEBOUNCE_MS);
}

// ---------------------------------------------------------------- connections

function ago(timestamp: number): string {
  const seconds = Math.max(0, (Date.now() - timestamp) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

function row(color: string, name: string, meta: string): HTMLLIElement {
  const item = document.createElement("li");
  const dot = document.createElement("span");
  dot.className = "dot";
  dot.style.setProperty("--who", color);
  const label = document.createElement("span");
  label.className = "name";
  label.textContent = name;
  const detail = document.createElement("span");
  detail.className = "meta";
  detail.textContent = meta;
  item.append(dot, label, detail);
  return item;
}

function agentLabel(a: { display_name: string; model: string | null; actor_id: string }) {
  const name = a.display_name.trim() || playfulName(seedFrom(a.actor_id));
  return a.model ? `${name} · ${toolReportedModelLabel(a.model)}` : name;
}

function empty(text: string): HTMLLIElement {
  const item = document.createElement("li");
  item.className = "none";
  item.textContent = text;
  return item;
}

function renderConnectionPeers() {
  if (!open) return;
  const { awareness, doc } = open;

  // Windows are *present* — awareness is live and never persisted.
  const peers = [...awareness.getStates().entries()];
  els.peers.replaceChildren(
    ...peers.map(([id, state]) => {
      const user = (state as { user?: { name: string; color: string } }).user;
      return row(
        user?.color ?? "#888",
        user?.name ?? "Someone",
        id === doc.clientID ? "this window" : "connected",
      );
    }),
  );
  if (peers.length === 0) els.peers.replaceChildren(empty("Not connected"));
}

async function refreshConnectionActors() {
  if (!open) return;
  const docId = openDocId;
  // Agents have *edited* — they come in over MCP, which carries no presence,
  // so this is history from the op log rather than who is attached right now.
  try {
    const actors = await mcp.documentActors(docId);
    if (!open || openDocId !== docId || els.connections.hidden) return;
    const agents = actors.filter((a) => a.kind === "agent");
    els.agents.replaceChildren(
      ...agents.map((a) =>
        row(
          // Fall back to a playful name only when a client sent none; renaming
          // something the user named would be worse than the problem.
          a.color || colorFor(seedFrom(a.actor_id)),
          agentLabel(a),
          `${a.edits} edit${a.edits === 1 ? "" : "s"} · ${ago(a.last_seen)}`,
        ),
      ),
    );
    if (agents.length === 0) {
      els.agents.replaceChildren(empty("No agent has edited this document"));
    }
  } catch {
    els.agents.replaceChildren(empty("Could not reach the daemon"));
  }
}

function toggleConnections(force?: boolean) {
  const show = force ?? els.connections.hidden;
  els.connections.hidden = !show;
  if (show) {
    renderConnectionPeers();
    void refreshConnectionActors();
  }
}

document.getElementById("status")!.addEventListener("click", (e) => {
  e.stopPropagation();
  toggleConnections();
});
document.addEventListener("mousedown", (e) => {
  if (!els.connections.hidden && !els.connections.contains(e.target as Node)) {
    toggleConnections(false);
  }
});
document.getElementById("new-window")!.addEventListener("click", () => {
  toggleConnections(false);
  void createNewDocument();
});
// ---------------------------------------------------------------- switcher

let results: DocumentSummary[] = [];
let selected = 0;
/** The switcher shows either live documents or the trash. */
let trashMode = false;

function renderHint() {
  els.hint.innerHTML = trashMode
    ? `<kbd>↵</kbd> open · <kbd>${ACCEL_LABEL}⌫</kbd> restore · <kbd>${ACCEL_LABEL}⇧⌫</kbd> back · <kbd>esc</kbd> close`
    : `<kbd>↵</kbd> open · <kbd>${ACCEL_LABEL}↵</kbd> new · <kbd>${ACCEL_LABEL}⌫</kbd> trash · <kbd>${ACCEL_LABEL}⇧⌫</kbd> view trash · <kbd>esc</kbd> close`;
  els.switcher.dataset.mode = trashMode ? "trash" : "live";
  els.input.placeholder = trashMode ? "Search the trash…" : "Search documents…";
}

function renderResults() {
  els.results.replaceChildren(
    ...results.map((row, i) => {
      const item = document.createElement("li");
      item.className = i === selected ? "hit selected" : "hit";
      item.textContent = row.title || "Untitled";
      item.addEventListener("mousedown", (e) => {
        e.preventDefault();
        choose(i);
      });
      return item;
    }),
  );
  if (results.length === 0) {
    const empty = document.createElement("li");
    empty.className = "empty";
    empty.textContent = trashMode
      ? "The trash is empty"
      : els.input.value
        ? `No matches — ${ACCEL_LABEL}↵ to create`
        : `No documents yet — ${ACCEL_LABEL}↵ to create`;
    els.results.replaceChildren(empty);
  }
}

async function refreshResults() {
  const query = els.input.value.trim();
  if (trashMode) {
    // Search runs over live documents only, so the trash filters by title.
    const all = await mcp.listDocuments(200, true);
    const needle = query.toLowerCase();
    results = needle
      ? all.filter((d) => d.title.toLowerCase().includes(needle))
      : all;
    selected = 0;
    renderResults();
    return;
  }
  if (query) {
    const hits = await mcp.search(query);
    results = hits.map((h) => ({ doc_id: h.doc_id, title: h.title, updated_at: 0 }));
  } else {
    results = await mcp.listDocuments();
  }
  selected = 0;
  renderResults();
}

function openSwitcher() {
  trashMode = false;
  renderHint();
  els.scrim.hidden = false;
  els.input.value = "";
  els.input.focus();
  void refreshResults();
}

function closeSwitcher() {
  els.scrim.hidden = true;
  open?.editor.commands.focus();
}

async function choose(index: number) {
  const row = results[index];
  if (!row) return;
  if (await openDocument(row.doc_id)) closeSwitcher();
}

async function toggleTrashMode() {
  trashMode = !trashMode;
  renderHint();
  els.input.value = "";
  els.input.focus();
  await refreshResults();
}

/**
 * Trash the highlighted document. Soft: the document and its history remain,
 * and the tombstone replicates, so this is undoable by anyone with the id.
 */
async function trashSelected() {
  const row = results[selected];
  if (!row) return;

  // In the trash the same key means the opposite thing: put it back.
  if (trashMode) {
    try {
      await editorApi.setDocumentDeleted(row.doc_id, false);
      notify(`Restored "${row.title || "Untitled"}"`);
      await refreshResults();
    } catch (error) {
      notify(`Could not restore: ${reason(error)}`, "error");
    }
    return;
  }

  const wasOpen = row.doc_id === openDocId;
  if (wasOpen && !(await canLeaveCurrentDocument())) return;
  try {
    await editorApi.setDocumentDeleted(row.doc_id, true);
    notify(`Moved "${row.title || "Untitled"}" to the trash · ${ACCEL_LABEL}⇧⌫ to find it`);
  } catch (error) {
    notify(`Could not trash: ${reason(error)}`, "error");
    return;
  }
  await refreshResults();

  // Do not leave the window staring at something that is no longer listed.
  if (wasOpen) {
    const next = results[0] ?? (await mcp.listDocuments())[0];
    if (next) {
      closeSwitcher();
      await openDocument(next.doc_id);
    }
  }
}

async function createDocumentInNewWindow(title: string) {
  // Browser development has no native window API and falls back to replacing
  // its preview editor, so it still needs the same durability guard.
  if (!isTauri() && !(await canLeaveCurrentDocument())) return;
  try {
    const created = await editorApi.createDocument(title);
    els.scrim.hidden = true;
    toggleConnections(false);
    try {
      await showDocumentInNewWindow(created.doc_id);
    } catch (error) {
      notify(
        `Document was created, but its window could not open: ${reason(error)}`,
        "error",
      );
    }
  } catch (error) {
    notify(`Could not create document: ${reason(error)}`, "error");
  }
}

async function createNewDocument() {
  await createDocumentInNewWindow("");
}

async function createFromQuery() {
  await createDocumentInNewWindow(els.input.value.trim());
}

async function importMarkdownFile() {
  els.scrim.hidden = true;
  toggleConnections(false);
  try {
    const file = await importMarkdownDocument(
      nativeFileBridge,
      editorApi,
      showDocumentInNewWindow,
    );
    if (file) notify(`Imported “${file.file_name}” as a new document`);
  } catch (error) {
    notify(`Could not import Markdown: ${reason(error)}`, "error");
  }
}

async function exportMarkdownFile(target = open): Promise<boolean> {
  if (!target) return false;
  try {
    // The live editor tree is the exact visible state. Exporting through it
    // also keeps the native file command independent from daemon transport.
    const exported = await exportMarkdownDocument(
      nativeFileBridge,
      target.editor.getJSON(),
      deriveTitle(target.editor),
    );
    if (!exported) return false;
    notify(`Exported “${exported.file_name}”`);
    return true;
  } catch (error) {
    notify(`Could not export Markdown: ${reason(error)}`, "error");
    return false;
  }
}

/**
 * Native windows are document-scoped. Browser development has no window API,
 * so it deliberately falls back to replacing the one preview editor.
 */
async function showDocumentInNewWindow(docId: string): Promise<void> {
  if (isTauri()) {
    await invoke("new_window", { docId });
    return;
  }
  await openDocument(docId);
}

// ---------------------------------------------------------------- keys

document.addEventListener("keydown", (event) => {
  if (
    accel(event) &&
    !event.shiftKey &&
    !event.altKey &&
    event.key.toLowerCase() === "n"
  ) {
    event.preventDefault();
    void createNewDocument();
    return;
  }
  if (
    accel(event) &&
    !event.shiftKey &&
    !event.altKey &&
    event.key.toLowerCase() === "o"
  ) {
    event.preventDefault();
    void importMarkdownFile();
    return;
  }
  if (
    accel(event) &&
    !event.shiftKey &&
    !event.altKey &&
    event.key.toLowerCase() === "s"
  ) {
    event.preventDefault();
    void exportMarkdownFile();
    return;
  }
  // Shift+accelerator+K belongs to the link editor. Keeping this exact avoids
  // the document switcher swallowing the more specific formatting shortcut.
  if (accel(event) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "k") {
    event.preventDefault();
    els.scrim.hidden ? openSwitcher() : closeSwitcher();
    return;
  }
  if (els.scrim.hidden) return;

  if (event.key === "Escape") {
    event.preventDefault();
    closeSwitcher();
  } else if (event.key === "Backspace" && accel(event) && event.shiftKey) {
    event.preventDefault();
    void toggleTrashMode();
  } else if (event.key === "Backspace" && accel(event)) {
    event.preventDefault();
    void trashSelected();
  } else if (event.key === "Enter" && accel(event)) {
    event.preventDefault();
    void createFromQuery();
  } else if (event.key === "Enter") {
    event.preventDefault();
    choose(selected);
  } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const step = event.key === "ArrowDown" ? 1 : -1;
    selected = Math.max(0, Math.min(results.length - 1, selected + step));
    renderResults();
  }
});

els.input.addEventListener("input", () => void refreshResults());
els.scrim.addEventListener("mousedown", (e) => {
  if (e.target === els.scrim) closeSwitcher();
});

// ---------------------------------------------------------------- boot

/** Daemon capabilities cross only the verified native bootstrap boundary. */
async function loadConnection(): Promise<Connection> {
  if (!isTauri()) {
    throw new Error(
      "Open Proof of Thought through the native app; browser-only development is not supported.",
    );
  }
  return invoke<Connection>("connection");
}

async function boot() {
  relabelShortcutHints();
  connection = await loadConnection();
  mcp = new Mcp(connection.mcp_url, connection.token);
  editorApi = new EditorApi(connection.mcp_url, connection.token);
  aiSupport.setReviewerBridge(reviewerBridge(editorApi));
  aiSupport.setConnectionCommand(connection.stdio_command);
  await mcp.connect();

  const documents = await mcp.listDocuments();
  const requested = new URL(window.location.href).searchParams.get("doc");
  const last = lastOpenDocument();
  let targetId: string;
  if (requested) {
    // The list is intentionally paginated for the switcher. A native document
    // window is pinned to its query id, so validate that id directly instead
    // of silently falling back when it is outside the first page.
    targetId = (await mcp.readDocument(requested)).doc_id;
  } else {
    targetId =
      documents.find((document) => document.doc_id === last)?.doc_id ??
      documents[0]?.doc_id ??
      (await editorApi.createDocument("")).doc_id;
  }

  await openDocument(targetId);
  if (requested) {
    // Keep the pin through all fallible startup work. A transient read or sync
    // failure must not turn Reload into a different document.
    window.history.replaceState(null, "", `${window.location.pathname}${window.location.hash}`);
  }
  await installNativeCloseGuard();
}

/** Only under Tauri; in a dev browser there is no native window to title. */
function getCurrentWindow() {
  return isTauri() ? tauriWindow() : null;
}

function isTauri(): boolean {
  return Boolean(
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__,
  );
}

async function installNativeCloseGuard(): Promise<void> {
  const nativeWindow = getCurrentWindow();
  if (!nativeWindow) return;
  await nativeWindow.onCloseRequested(async (event) => {
    const provider = open?.provider;
    if (!provider?.hasPendingChanges) return;

    // Native close requests are cancellable only synchronously. Keep the
    // window alive until the daemon confirms that SQLite has the latest edit.
    event.preventDefault();
    if (closingAfterAutosave) return;
    closingAfterAutosave = true;
    try {
      const saved = await provider.waitUntilSaved();
      if (!saved || open?.provider !== provider || provider.hasPendingChanges) {
        notify(
          "Window kept open because changes are still waiting to autosave.",
          "error",
        );
        return;
      }

      // `destroy` skips a second close-request event after the durability
      // barrier has passed.
      await nativeWindow.destroy();
    } catch (error) {
      notify(`Could not close window: ${reason(error)}`, "error");
    } finally {
      closingAfterAutosave = false;
    }
  });
}

window.addEventListener("beforeunload", (event) => {
  if (!open?.provider.hasPendingChanges) return;
  event.preventDefault();
});

boot().catch((error) => {
  document.title = "Could not reach the daemon";
  notify(`Could not reach the daemon: ${reason(error)}`, "error");
  console.error(error);
});
