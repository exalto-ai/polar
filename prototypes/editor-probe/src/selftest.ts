/**
 * Automated passes, so the probe reports the same verdict in WKWebView as in
 * Chromium without a debugger attached. IME is deliberately absent —
 * composition cannot be synthesised faithfully and stays a manual pass.
 *
 * Waits poll for convergence rather than sleeping a fixed duration: under load
 * the main thread falls badly behind the nominal link latency, so a fixed sleep
 * measures the sleep, not the system. Time-to-converge is itself a result.
 */
import type { Editor } from "@tiptap/core";

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
const norm = (e: Editor) => e.getText().replace(/\s+/g, " ").trim();

export type Result = { name: string; pass: boolean; detail: string };

export type Probe = {
  editorA: Editor;
  editorB: Editor;
  net: { latency: number; online: boolean };
  M: { caretJumps: number; imeCollisions: number };
  agentBurst: (n: number) => void;
  setOnline: (v: boolean) => void;
};

/** Resolves when the predicate holds, or gives up. Returns ms elapsed, or null. */
async function waitFor(pred: () => boolean, timeoutMs: number): Promise<number | null> {
  const t0 = performance.now();
  while (performance.now() - t0 < timeoutMs) {
    if (pred()) return Math.round(performance.now() - t0);
    await wait(120);
  }
  return null;
}

export async function runSelfTest(P: Probe, onPhase: (s: string) => void): Promise<Result[]> {
  const out: Result[] = [];
  const converged = () => norm(P.editorA) === norm(P.editorB);
  const caretBefore = P.M.caretJumps;

  // 1 — convergence under a heavy agent burst with a live local caret
  onPhase("agent burst under a live caret");
  P.editorA.commands.focus();
  P.editorA.commands.setTextSelection(40);
  // Sample throughout: converging without ever having diverged is a vacuous
  // pass, and that is exactly what a naive check at t=0 reports.
  let sawDivergence = false;
  const sampler = window.setInterval(() => {
    if (!converged()) sawDivergence = true;
  }, 80);

  P.agentBurst(60);
  for (let i = 0; i < 25; i++) {
    P.editorA.commands.insertContentAt(1, `h${i} `);
    await wait(20);
  }
  {
    const ms = await waitFor(() => sawDivergence && converged(), 45000);
    window.clearInterval(sampler);
    out.push({
      name: "Converges under 60 concurrent agent ops",
      pass: ms !== null,
      detail: ms === null
        ? sawDivergence
          ? `still divergent after 45s (A=${norm(P.editorA).length} B=${norm(P.editorB).length})`
          : "replicas never diverged — test vacuous"
        : `${norm(P.editorA).length} chars, diverged then settled in ${ms}ms`,
    });
  }

  // 2 — identical is not enough; the tree has to be schema-valid
  onPhase("structural validation");
  let valid = true, why = "schema-valid on both replicas";
  try {
    P.editorA.state.doc.check();
    P.editorB.state.doc.check();
  } catch (e) {
    valid = false;
    why = String(e);
  }
  out.push({ name: "ProseMirror doc passes check()", pass: valid, detail: why });

  // 3 — offline divergence, then reconciliation
  onPhase("offline divergence");
  P.setOnline(false);
  P.agentBurst(25);
  for (let i = 0; i < 25; i++) {
    P.editorA.commands.insertContentAt(1, `off${i} `);
    await wait(20);
  }
  await wait(1500);
  const dA = norm(P.editorA).length, dB = norm(P.editorB).length;

  onPhase("reconnecting");
  P.setOnline(true);
  {
    const ms = await waitFor(converged, 45000);
    out.push({
      name: "Offline divergence reconciles",
      pass: ms !== null && dA !== dB,
      detail: dA === dB
        ? "replicas never diverged — test invalid"
        : ms === null
          ? `diverged ${dA}/${dB}, never reconciled`
          : `diverged ${dA}/${dB} → merged in ${ms}ms`,
    });
  }

  // 4 — a remote update must never steal the caret
  const jumps = P.M.caretJumps - caretBefore;
  out.push({
    name: "Caret survives remote updates",
    pass: jumps === 0,
    detail: jumps === 0 ? "no unexplained caret movement" : `${jumps} caret jumps`,
  });

  // 5 — markdown input rules still fire (the Bear-feel affordance, AD-3)
  onPhase("input rules");
  // Assert on the heading COUNT, not on lastChild: TipTap keeps a trailing empty
  // paragraph, so the converted block is never the last node. Earlier versions of
  // this check failed for that reason alone and looked like an engine bug.
  //
  // Typing through the engine (execCommand -> beforeinput -> ProseMirror's DOM
  // observer) is the faithful path but needs real DOM focus, which a
  // backgrounded window lacks. Fall back to driving the rule plugins directly
  // and say which path ran, rather than reporting a focus problem as a failure.
  const countHeadings = () => {
    let n = 0;
    P.editorA.state.doc.descendants((node) => {
      if (node.type.name === "heading") n++;
    });
    return n;
  };

  const headingsBefore = countHeadings();
  P.editorA.commands.insertContentAt(P.editorA.state.doc.content.size, {
    type: "paragraph",
  });
  await wait(80);
  const last = P.editorA.state.doc.lastChild;
  const scratchOk = last?.type.name === "paragraph" && last.content.size === 0;
  const start = P.editorA.state.doc.content.size - (last?.nodeSize ?? 0) + 1;
  // setTextSelection, not focus(): the rule handler acts on the selection, and
  // focus() cannot succeed without DOM focus.
  P.editorA.commands.setTextSelection(start);
  P.editorA.commands.focus(start);
  await wait(80);

  const focused = P.editorA.view.hasFocus();
  const path = focused ? "real typing" : "rule plugin (window unfocused)";

  if (focused) {
    // One character at a time: a two-character insert arrives as a single DOM
    // mutation that never reaches handleTextInput.
    document.execCommand("insertText", false, "#");
    await wait(120);
    document.execCommand("insertText", false, " ");
    await wait(400);
  } else {
    // someProp() is unusable here: TipTap registers one handleTextInput plugin
    // per extension and the first to claim the input wins, rarely the heading rule.
    for (const plugin of P.editorA.state.plugins) {
      const handler = (plugin as any).props?.handleTextInput;
      if (!handler) continue;
      handler(P.editorA.view, start, start, "# ");
      await wait(40);
      if (countHeadings() > headingsBefore) break;
    }
  }

  const headingsAfter = countHeadings();
  out.push({
    name: "Markdown input rules fire",
    pass: scratchOk && headingsAfter === headingsBefore + 1,
    detail: !scratchOk
      ? `scratch block not created (last=<${last?.type.name}>) — inconclusive`
      : `via ${path}: headings ${headingsBefore} -> ${headingsAfter}`,
  });

  return out;
}

export function renderResults(results: Result[]) {
  const host = document.getElementById("selftest")!;
  const passed = results.filter((r) => r.pass).length;
  const all = passed === results.length;
  host.hidden = false;
  host.innerHTML = `
    <div class="st-head ${all ? "ok" : "bad"}">
      ${all ? "PASS" : "FAIL"} · ${passed}/${results.length} automated checks
      <span>IME remains a manual pass — see the checklist</span>
    </div>
    ${results.map((r) => `<div class="st-row ${r.pass ? "ok" : "bad"}">
        <span class="st-mark">${r.pass ? "✓" : "✕"}</span>
        <span class="st-name"></span>
        <span class="st-detail"></span>
      </div>`).join("")}`;
  host.querySelectorAll(".st-row").forEach((row, i) => {
    row.querySelector(".st-name")!.textContent = results[i].name;
    row.querySelector(".st-detail")!.textContent = results[i].detail;
  });
}
