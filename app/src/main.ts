/**
 * The window. Opens one document at a time; ⌘K switches.
 */
import { invoke } from "@tauri-apps/api/core";
import * as Y from "yjs";
import { Awareness } from "y-protocols/awareness";
import type { Editor } from "@tiptap/core";
import { createEditor } from "./editor";
import { Mcp, type DocumentSummary } from "./mcp";
import { SyncProvider, type ProviderStatus } from "./provider";

type Connection = {
  sync_url: string;
  mcp_url: string;
  token: string;
  stdio_command: string;
};

const PALETTE = ["#4c8dff", "#e0a44a", "#b98cff", "#5ac88f", "#ff7a6b"];

const els = {
  title: document.getElementById("title")!,
  status: document.getElementById("status")!,
  presence: document.getElementById("presence")!,
  editor: document.getElementById("editor")!,
  scrim: document.getElementById("scrim")!,
  connections: document.getElementById("connections")!,
  peers: document.getElementById("peers")!,
  agents: document.getElementById("agents")!,
  stdioCommand: document.getElementById("stdio-command")!,
  input: document.getElementById("switcher-input") as HTMLInputElement,
  results: document.getElementById("switcher-results")!,
};

let connection: Connection;
let mcp: Mcp;
let open: { doc: Y.Doc; awareness: Awareness; provider: SyncProvider; editor: Editor } | null =
  null;
let openDocId = "";

function setStatus(status: ProviderStatus) {
  els.status.dataset.state = status;
  els.status.title = status;
}

/** The title is the first heading, matching what the daemon derives. */
function refreshTitle(editor: Editor) {
  const heading = editor.state.doc.content.firstChild;
  const text = heading?.textContent?.trim();
  els.title.textContent = text && text.length > 0 ? text : "Untitled";
}

function renderPresence(awareness: Awareness, self: number) {
  const others = [...awareness.getStates().entries()].filter(([id]) => id !== self);
  els.presence.replaceChildren(
    ...others.map(([, state]) => {
      const user = (state as { user?: { name: string; color: string } }).user;
      const chip = document.createElement("span");
      chip.className = "who";
      chip.style.setProperty("--who", user?.color ?? "#888");
      chip.textContent = (user?.name ?? "?").slice(0, 1).toUpperCase();
      chip.title = user?.name ?? "unknown";
      return chip;
    }),
  );
}

async function openDocument(docId: string) {
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
  );

  // Distinct per window so two windows are visibly different peers.
  const user = {
    name: `Window ${(doc.clientID % 97).toString().padStart(2, "0")}`,
    color: PALETTE[doc.clientID % PALETTE.length],
  };
  const editor = createEditor(els.editor, doc, awareness, provider, user);
  awareness.setLocalStateField("user", user);

  provider.connect();
  open = { doc, awareness, provider, editor };
  openDocId = docId;

  editor.on("update", () => refreshTitle(editor));
  awareness.on("change", () => {
    renderPresence(awareness, doc.clientID);
    if (!els.connections.hidden) void renderConnections();
  });
  refreshTitle(editor);
  window.localStorage.setItem("polar.last", docId);
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

function empty(text: string): HTMLLIElement {
  const item = document.createElement("li");
  item.className = "none";
  item.textContent = text;
  return item;
}

async function renderConnections() {
  if (!open) return;
  const { awareness, doc } = open;

  // Windows are *present* — awareness is live and never persisted.
  const peers = [...awareness.getStates().entries()];
  els.peers.replaceChildren(
    ...peers.map(([id, state]) => {
      const user = (state as { user?: { name: string; color: string } }).user;
      return row(
        user?.color ?? "#888",
        user?.name ?? "Unknown",
        id === doc.clientID ? "this window" : "connected",
      );
    }),
  );
  if (peers.length === 0) els.peers.replaceChildren(empty("Not connected"));

  // Agents have *edited* — they come in over MCP, which carries no presence,
  // so this is history from the op log rather than who is attached right now.
  try {
    const actors = await mcp.documentActors(openDocId);
    const agents = actors.filter((a) => a.kind === "agent");
    els.agents.replaceChildren(
      ...agents.map((a) =>
        row(
          a.color,
          a.model ? `${a.display_name} · ${a.model}` : a.display_name,
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
  if (show) void renderConnections();
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
  void invoke("new_window");
});
document.getElementById("copy-command")!.addEventListener("click", async (e) => {
  await navigator.clipboard.writeText(connection.stdio_command);
  const button = e.currentTarget as HTMLButtonElement;
  button.textContent = "Copied";
  setTimeout(() => (button.textContent = "Copy"), 1200);
});

// ---------------------------------------------------------------- switcher

let results: DocumentSummary[] = [];
let selected = 0;

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
    empty.textContent = els.input.value
      ? "No matches — ⌘↵ to create"
      : "No documents yet — ⌘↵ to create";
    els.results.replaceChildren(empty);
  }
}

async function refreshResults() {
  const query = els.input.value.trim();
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
  els.scrim.hidden = false;
  els.input.value = "";
  els.input.focus();
  void refreshResults();
}

function closeSwitcher() {
  els.scrim.hidden = true;
  open?.editor.commands.focus();
}

function choose(index: number) {
  const row = results[index];
  if (!row) return;
  closeSwitcher();
  void openDocument(row.doc_id);
}

async function createFromQuery() {
  const title = els.input.value.trim() || "Untitled";
  const created = await mcp.createDocument(title);
  closeSwitcher();
  await openDocument(created.doc_id);
}

// ---------------------------------------------------------------- keys

document.addEventListener("keydown", (event) => {
  if (event.metaKey && event.key.toLowerCase() === "n") {
    event.preventDefault();
    void invoke("new_window");
    return;
  }
  if (event.metaKey && event.key.toLowerCase() === "k") {
    event.preventDefault();
    els.scrim.hidden ? openSwitcher() : closeSwitcher();
    return;
  }
  if (els.scrim.hidden) return;

  if (event.key === "Escape") {
    event.preventDefault();
    closeSwitcher();
  } else if (event.key === "Enter" && event.metaKey) {
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

/** Under Tauri the daemon details come from Rust; in a dev browser, from Vite. */
async function loadConnection(): Promise<Connection> {
  if ((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__) {
    return invoke<Connection>("connection");
  }
  const response = await fetch("/__polar/connection");
  if (!response.ok) throw new Error("no daemon is running");
  return response.json();
}

async function boot() {
  connection = await loadConnection();
  mcp = new Mcp(connection.mcp_url, connection.token);
  els.stdioCommand.textContent = connection.stdio_command;
  await mcp.connect();

  const documents = await mcp.listDocuments();
  const last = window.localStorage.getItem("polar.last");
  const target =
    documents.find((d) => d.doc_id === last) ??
    documents[0] ??
    (await mcp.createDocument("Untitled"));

  await openDocument("doc_id" in target ? target.doc_id : (target as any).doc_id);
}

boot().catch((error) => {
  els.title.textContent = "Could not reach the daemon";
  console.error(error);
});
