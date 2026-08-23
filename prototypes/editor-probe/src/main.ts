/**
 * Polar editor probe — exists to answer one question (AD-8):
 * can WKWebView host a collaborative ProseMirror well enough to build on?
 *
 * Two Y.Docs, a fake link with adjustable latency and an offline queue, and an
 * agent that fires realistic block-level edits. Everything that could plausibly
 * break in WKWebView is instrumented rather than left to the eye — especially
 * remote updates landing mid-IME-composition, which is the failure that would
 * actually invalidate the stack.
 */
import * as Y from "yjs";
import {
  Awareness,
  encodeAwarenessUpdate,
  applyAwarenessUpdate,
} from "y-protocols/awareness";
import { Editor } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import Collaboration from "@tiptap/extension-collaboration";
import CollaborationCaret from "@tiptap/extension-collaboration-caret";
import { runSelfTest, renderResults } from "./selftest";
import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------- metrics

const M = {
  remoteUpdates: 0,
  imeCollisions: 0,
  caretJumps: 0,
  agentOps: 0,
  queued: 0,
};

const metricDefs: [keyof typeof M, string, boolean][] = [
  ["remoteUpdates", "remote updates", false],
  ["agentOps", "agent ops", false],
  ["imeCollisions", "IME collisions", true],
  ["caretJumps", "caret jumps", true],
];

function renderMetrics() {
  const el = document.getElementById("metrics")!;
  el.innerHTML = metricDefs
    .map(([k, label, bad]) => {
      const v = M[k];
      const cls = bad && v > 0 ? "metric alarm" : "metric";
      return `<div class="${cls}"><b>${v}</b><span>${label}</span></div>`;
    })
    .join("");
  document.getElementById("queued")!.textContent = `${M.queued} queued`;
}

// ---------------------------------------------------------------- log

const logEl = document.getElementById("log")!;
let logCount = 0;

function log(who: string, msg: string, kind: "info" | "warn" | "good" = "info") {
  logCount++;
  const row = document.createElement("div");
  row.className = `row ${kind}`;
  const t = new Date();
  const ts = `${String(t.getMinutes()).padStart(2, "0")}:${String(
    t.getSeconds()
  ).padStart(2, "0")}.${String(t.getMilliseconds()).padStart(3, "0")}`;
  row.innerHTML = `<span class="t">${ts}</span><span class="w ${who.toLowerCase()}">${who}</span><span class="m"></span>`;
  row.querySelector(".m")!.textContent = msg;
  logEl.prepend(row);
  while (logEl.childElementCount > 300) logEl.lastElementChild!.remove();
  if (logCount % 5 === 0) renderMetrics();
}

// ---------------------------------------------------------------- network

const net = { latency: 120, online: true };

type Pending = { update: Uint8Array; to: Y.Doc };
const offlineQueue: Pending[] = [];

function linkDocs(from: Y.Doc, to: Y.Doc, label: string) {
  from.on("update", (update: Uint8Array, origin: unknown) => {
    if (origin === "net") return; // never echo what the link delivered
    if (!net.online) {
      offlineQueue.push({ update, to });
      M.queued = offlineQueue.length;
      renderMetrics();
      return;
    }
    deliver({ update, to }, label);
  });
}

function deliver(p: Pending, label: string) {
  window.setTimeout(() => {
    Y.applyUpdate(p.to, p.update, "net");
    M.remoteUpdates++;
    if (M.remoteUpdates % 10 === 0) log(label, `${M.remoteUpdates} remote updates applied`);
  }, net.latency);
}

function linkAwareness(from: Awareness, to: Awareness) {
  from.on("update", (changes: any, origin: unknown) => {
    if (origin === "net") return;
    const ids = [...changes.added, ...changes.updated, ...changes.removed];
    const payload = encodeAwarenessUpdate(from, ids);
    window.setTimeout(() => {
      if (net.online) applyAwarenessUpdate(to, payload, "net");
    }, net.latency);
  });
}

// ---------------------------------------------------------------- docs

const docA = new Y.Doc();
const docB = new Y.Doc();
const awA = new Awareness(docA);
const awB = new Awareness(docB);

const SEED = `<h1>The shape of a document</h1>
<p>This paragraph exists so there is something to collide with. Put your caret in the middle of it, then run an agent burst and watch whether the caret holds its position.</p>
<p>A second paragraph, because block boundaries are where selection handling tends to fall apart. Try shift-arrowing across this edge while remote edits are landing.</p>
<blockquote><p>Formatting is metadata on a range, not characters in a string. That is the whole argument for the structured tree.</p></blockquote>
<ul><li><p>Input rules should still fire under latency.</p></li><li><p>Undo should stay scoped to your own edits.</p></li></ul>`;

function makeEditor(
  mount: string,
  doc: Y.Doc,
  awareness: Awareness,
  user: { name: string; color: string }
) {
  return new Editor({
    element: document.querySelector(mount)!,
    extensions: [
      // Yjs owns history; StarterKit's undoRedo would fight it.
      StarterKit.configure({ undoRedo: false }),
      Collaboration.configure({ fragment: doc.getXmlFragment("content") }),
      CollaborationCaret.configure({ provider: { awareness } as any, user }),
    ],
  });
}

const editorA = makeEditor("#editor-a", docA, awA, { name: "You", color: "#4c8dff" });
const editorB = makeEditor("#editor-b", docB, awB, { name: "Peer", color: "#e0a44a" });

// Seed one side only, then bring the other up to date, then open the link.
// Seeding both would duplicate content instead of converging.
editorA.commands.setContent(SEED);
Y.applyUpdate(docB, Y.encodeStateAsUpdate(docA), "net");
linkDocs(docA, docB, "Peer");
linkDocs(docB, docA, "You");
linkAwareness(awA, awB);
linkAwareness(awB, awA);

// ---------------------------------------------------------------- instrumentation

/**
 * The measurement that matters. WKWebView's contenteditable is weakest around
 * IME composition; if a remote update applied mid-composition kills the
 * candidate window or drops the buffer, the stack is wrong and we want to know
 * now rather than after the schema is written.
 */
function instrument(name: string, editor: Editor, doc: Y.Doc) {
  const dom = editor.view.dom;
  let composing = false;
  let buffer = "";

  dom.addEventListener("compositionstart", () => {
    composing = true;
    buffer = "";
    log(name, "compositionstart");
  });
  dom.addEventListener("compositionupdate", (e) => {
    buffer = (e as CompositionEvent).data ?? "";
  });
  dom.addEventListener("compositionend", (e) => {
    composing = false;
    const data = (e as CompositionEvent).data ?? "";
    log(name, `compositionend — committed "${data}"`, data ? "good" : "warn");
  });

  doc.on("update", (_u: Uint8Array, origin: unknown) => {
    if (origin !== "net") return;

    if (composing) {
      M.imeCollisions++;
      log(
        name,
        `remote update applied during IME composition (buffer "${buffer}") — check the candidate window survived`,
        "warn"
      );
    }

    const before = editor.state.selection.anchor;
    const wasFocused = editor.view.hasFocus();
    requestAnimationFrame(() => {
      const after = editor.state.selection.anchor;
      // Only meaningful while focused; an unfocused pane has no caret to lose.
      if (wasFocused && before !== after) {
        M.caretJumps++;
        log(name, `caret moved ${before} → ${after} on a remote update`, "warn");
      }
      renderMetrics();
    });
  });
}

instrument("You", editorA, docA);
instrument("Peer", editorB, docB);

// ---------------------------------------------------------------- agent

const AGENT_TEXT = [
  "Rewritten by an agent: the merge should stay structurally valid under this.",
  "An agent replaced this block while a human was typing somewhere above it.",
  "Concurrent structural edits are the case flat markdown cannot survive.",
  "This sentence arrived from a different actor with its own client ID.",
];

/**
 * Realistic agent traffic: block-level replacements and insertions, the shape
 * the MCP tool surface will actually emit. Deliberately never calls focus() —
 * an agent stealing focus mid-composition would be our bug, not WKWebView's.
 */
function agentBurst(count: number) {
  const gap = 40;
  for (let i = 0; i < count; i++) {
    window.setTimeout(() => {
      const paras: { pos: number; size: number }[] = [];
      editorB.state.doc.descendants((node, pos) => {
        if (node.type.name === "paragraph" && node.content.size > 0) {
          paras.push({ pos, size: node.content.size });
        }
      });

      const text = AGENT_TEXT[Math.floor(Math.random() * AGENT_TEXT.length)];

      if (paras.length === 0 || Math.random() < 0.2) {
        editorB.commands.insertContentAt(
          editorB.state.doc.content.size,
          `<p>${text}</p>`
        );
      } else {
        const t = paras[Math.floor(Math.random() * paras.length)];
        editorB.commands.insertContentAt(
          { from: t.pos + 1, to: t.pos + 1 + t.size },
          text
        );
      }
      M.agentOps++;
      if (i === count - 1) {
        log("Agent", `burst of ${count} block ops complete`);
        renderMetrics();
      }
    }, i * gap);
  }
  log("Agent", `starting burst — ${count} ops at ${gap}ms, ${net.latency}ms link`);
}

// ---------------------------------------------------------------- controls

const latEl = document.getElementById("latency") as HTMLInputElement;
latEl.addEventListener("input", () => {
  net.latency = Number(latEl.value);
  document.getElementById("lat-val")!.textContent = latEl.value;
});

const offlineBtn = document.getElementById("offline")!;

/**
 * Single owner of link state. The self-test drives this rather than poking
 * net.online, so the queue flush cannot be bypassed.
 */
function setOnline(v: boolean) {
  if (net.online === v) return;
  net.online = v;
  offlineBtn.textContent = v ? "Go offline" : "Go online";
  offlineBtn.classList.toggle("armed", !v);
  if (v) {
    const n = offlineQueue.length;
    while (offlineQueue.length) deliver(offlineQueue.shift()!, "sync");
    M.queued = 0;
    log("Link", `back online — flushed ${n} queued updates`, "good");
  } else {
    log("Link", "offline — updates will queue on both replicas", "warn");
  }
  renderMetrics();
}

offlineBtn.addEventListener("click", () => setOnline(!net.online));

document.getElementById("burst")!.addEventListener("click", () => {
  agentBurst(Number((document.getElementById("ops") as HTMLSelectElement).value));
});

document.getElementById("reset")!.addEventListener("click", () => location.reload());
document.getElementById("clear")!.addEventListener("click", () => {
  logEl.innerHTML = "";
});

// ---------------------------------------------------------------- engine badge

const ua = navigator.userAgent;
const isWK = /AppleWebKit/.test(ua) && !/Chrome|Chromium|Edg/.test(ua);
const engineEl = document.getElementById("engine")!;
engineEl.textContent = isWK ? "WKWebView / Safari" : "Chromium";
engineEl.className = `engine ${isWK ? "wk" : "chrome"}`;

renderMetrics();
log("Probe", `ready — ${isWK ? "WKWebView" : "Chromium"} · TipTap 3 · Yjs 13`, "good");

// Exposed for driving the probe from a console or an automated pass.
const probe = { editorA, editorB, docA, docB, net, M, agentBurst, setOnline };
(window as any).__probe = probe;

const selfTestBtn = document.getElementById("selftest-run")!;
async function selfTest() {
  selfTestBtn.setAttribute("disabled", "true");
  selfTestBtn.textContent = "Running…";
  log("Probe", "self-test started — ~20s", "good");
  try {
    const results = await runSelfTest(probe, (phase) => {
      selfTestBtn.textContent = "Running…";
      log("Probe", `phase: ${phase}`);
    });
    renderResults(results);
    log("Probe", "self-test complete", "good");
    // Under Tauri, hand the verdict to Rust so it lands on stdout.
    if ((window as any).__TAURI_INTERNALS__) {
      await invoke("report", {
        payload: JSON.stringify(
          { engine: engineEl.textContent, ua: navigator.userAgent, results },
          null,
          2
        ),
      });
    }
  } catch (e) {
    log("Probe", `self-test threw: ${e}`, "warn");
  }
  selfTestBtn.removeAttribute("disabled");
  selfTestBtn.textContent = "Self-test";
  renderMetrics();
}
selfTestBtn.addEventListener("click", selfTest);
if (new URLSearchParams(location.search).has("autotest")) setTimeout(selfTest, 400);
