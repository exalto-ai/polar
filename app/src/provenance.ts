/**
 * The provenance rails: who wrote which block, in the left margin.
 *
 * Yjs cannot carry authorship (AD-1), so this comes from the op log by way of
 * the daemon's `block_provenance` tool — the same tool an agent can call to ask
 * where a paragraph came from. AD-6's insistence on identity from the first
 * commit is what makes it answerable at all.
 *
 * Two decisions worth stating, because both were choices:
 *
 * **Nothing is drawn for a document you wrote alone.** Rails answer "who else
 * has been in here", and striping a solo document permanently to tell the user
 * something they already know is the failure mode this app is built against.
 * The moment a second actor appears, the user's own blocks are drawn too — but
 * fainter, because by then the question is which parts are *not* theirs.
 *
 * **Blocks with no entry get no rail.** Unattributed is not the same as yours,
 * and a document synced from a relay arrives with content and no log.
 */
import type { Editor } from "@tiptap/core";
import type * as Y from "yjs";
import type { BlockAttribution } from "./mcp";
import { colorFor, seedFrom } from "./names";

/**
 * A block's id, as the daemon spells it.
 *
 * Block identity is the yrs `BranchID` — `client:clock` of the item that
 * created the element — and `thought_core::block::block_id` formats it exactly
 * this way. Yjs exposes the same id on `_item`, which is internal enough to be
 * worth pinning in a test: if this drifts, ids stop matching and rails vanish
 * rather than landing on the wrong block, which is the failure to prefer.
 */
type WithItem = { _item?: { id: { client: number; clock: number } } | null };

export function blockIdOf(node: unknown): string | null {
  const item = (node as WithItem)?._item;
  return item ? `${item.id.client}:${item.id.clock}` : null;
}

/** A top-level block as the CRDT holds it. */
export type YBlock = { id: string | null; kind: string | null };

/**
 * Line the CRDT's blocks up with the editor's, or refuse to.
 *
 * These two are *nearly* the same list and the difference is not a bug: TipTap
 * keeps a trailing empty paragraph so there is always somewhere to click below
 * the last block, and that node never enters the CRDT. So the editor legitimately
 * runs one or more nodes longer, and a plain length comparison rejects every
 * frame forever.
 *
 * Matching by position is still only safe while the two agree about what is at
 * each position, which they can briefly not — y-prosemirror applies remote
 * changes to Yjs and ProseMirror in that order. Comparing node kinds is a cheap
 * way to notice, and returning `null` skips the frame: a rail that is late is a
 * rail nobody notices, and a rail on the wrong paragraph is a lie.
 */
export function alignBlocks(yBlocks: YBlock[], editorKinds: string[]): (string | null)[] | null {
  if (editorKinds.length < yBlocks.length) return null;
  for (const [index, block] of yBlocks.entries()) {
    if (block.kind !== editorKinds[index]) return null;
  }
  return yBlocks.map((block) => block.id);
}

/** One drawn bar: a run of adjacent blocks by the same actor. */
export type Run = {
  actorId: string;
  from: number;
  to: number;
};

/**
 * Group adjacent blocks by the actor that last touched them.
 *
 * A run of paragraphs from one agent reads as one edit, so it is drawn as one
 * continuous bar rather than a stack of ticks that implies more separate acts
 * than actually happened.
 */
export function runsOf(actorIds: (string | null)[]): Run[] {
  const runs: Run[] = [];
  actorIds.forEach((actorId, index) => {
    if (!actorId) return;
    const last = runs[runs.length - 1];
    if (last && last.actorId === actorId && last.to === index - 1) last.to = index;
    else runs.push({ actorId, from: index, to: index });
  });
  return runs;
}

function ago(timestamp: number): string {
  const seconds = Math.max(0, (Date.now() - timestamp) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

/**
 * What the label says.
 *
 * `created_by` is carried separately from `touched_by` precisely so this can be
 * honest about a paragraph an agent drafted and a human then reworded — saying
 * only "you" there would erase where the words came from.
 */
export function labelFor(block: BlockAttribution, selfId: string): string {
  const who = block.touched_by === selfId ? "You" : block.display_name || block.touched_by;
  const model = block.touched_by !== selfId && block.model ? ` · ${block.model}` : "";
  const drafted =
    block.created_by !== block.touched_by
      ? `, drafted by ${block.created_by === selfId ? "you" : shortName(block.created_by)}`
      : "";
  return `${who}${model} · ${ago(block.touched_at)}${drafted}`;
}

/** `agent:opus` reads as `opus`; the prefix is plumbing, not a name. */
function shortName(actorId: string): string {
  const at = actorId.indexOf(":");
  return at === -1 ? actorId : actorId.slice(at + 1);
}

export type Rails = {
  /** Replace what the daemon last said about this document. */
  setProvenance(blocks: BlockAttribution[]): void;
  /** Light up one actor's rails — the presence chips point with this. */
  highlight(actorId: string | null): void;
  destroy(): void;
};

/** Backstop for when the window is hidden and no frames are painted. */
const HIDDEN_REDRAW_MS = 50;

export function installProvenanceRails(
  editor: Editor,
  ydoc: Y.Doc,
  container: HTMLElement,
  selfId: string,
): Rails {
  const layer = document.createElement("div");
  layer.className = "rails";
  layer.setAttribute("aria-hidden", "true");
  container.append(layer);

  const label = document.createElement("div");
  label.className = "rail-label";
  label.hidden = true;
  layer.append(label);

  let attribution = new Map<string, BlockAttribution>();
  let frame: number | null = null;
  let timer: number | null = null;

  /** Blocks in document order, straight from the CRDT. */
  function yBlocks(): YBlock[] {
    return ydoc
      .getXmlFragment("content")
      .toArray()
      .map((node) => ({
        id: blockIdOf(node),
        kind: (node as { nodeName?: string }).nodeName ?? null,
      }));
  }

  function draw() {
    frame = null;
    const blocks = [...editor.view.dom.children] as HTMLElement[];
    const kinds: string[] = [];
    editor.state.doc.forEach((node) => kinds.push(node.type.name));

    // Keeping the last frame beats drawing against blocks that have moved; the
    // next update redraws anyway.
    const ids = alignBlocks(yBlocks(), kinds);
    if (!ids || blocks.length < ids.length) return;

    const touched = ids.map((id) => (id ? (attribution.get(id)?.touched_by ?? null) : null));
    const others = new Set(touched.filter((who) => who && who !== selfId));

    // A document only this window has written needs no explanation.
    if (others.size === 0) {
      layer.replaceChildren(label);
      return;
    }

    const rails = runsOf(touched).map((run) => {
      const first = blocks[run.from];
      const last = blocks[run.to];
      const block = attribution.get(ids[run.from]!)!;
      const isSelf = run.actorId === selfId;

      const rail = document.createElement("div");
      rail.className = "rail";
      rail.dataset.actor = run.actorId;
      if (isSelf) rail.dataset.self = "";
      // An agent is not a person, and colour alone makes that a memory game —
      // so the two are told apart by how the bar is drawn as well.
      if (block.kind === "agent") rail.dataset.agent = "";
      rail.style.setProperty("--who", block.color || colorFor(seedFrom(run.actorId)));
      rail.style.top = `${first.offsetTop}px`;
      rail.style.height = `${last.offsetTop + last.offsetHeight - first.offsetTop}px`;

      rail.addEventListener("mouseenter", () => {
        label.textContent = labelFor(block, selfId);
        label.hidden = false;
        // Sits in the gap *above* the block it describes, measured rather than
        // guessed. Placed level with the block it instead cut through the last
        // line of the block before, which reads as a rendering fault rather
        // than a label. Clamped at the top of the document, where there is no
        // gap to sit in.
        const clear = label.offsetHeight + 4;
        label.style.top = `${Math.max(first.offsetTop - clear, 0)}px`;
        rail.dataset.hover = "";
      });
      rail.addEventListener("mouseleave", () => {
        label.hidden = true;
        delete rail.dataset.hover;
      });
      return rail;
    });

    layer.replaceChildren(label, ...rails);
  }

  /**
   * Coalesced into a frame: every keystroke reflows the blocks below it, and
   * measuring on each one would put a layout read in the typing path.
   */
  /**
   * Redraw on the next frame, or on a timer — whichever comes first.
   *
   * `requestAnimationFrame` does not fire in a hidden window, so a frame-only
   * schedule leaves the rails stale for as long as the window sits behind something
   * else: an agent rewrites three paragraphs, and the margin still credits
   * whoever wrote them last week until the window is looked at again. Same
   * failure the sync provider had, and the same fix.
   */
  function schedule() {
    if (frame !== null || timer !== null) return;
    const run = () => {
      if (frame !== null) cancelAnimationFrame(frame);
      if (timer !== null) clearTimeout(timer);
      frame = null;
      timer = null;
      draw();
    };
    frame = requestAnimationFrame(run);
    timer = window.setTimeout(run, HIDDEN_REDRAW_MS);
  }

  editor.on("update", schedule);
  window.addEventListener("resize", schedule);

  return {
    setProvenance(blocks) {
      attribution = new Map(blocks.map((b) => [b.block_id, b]));
      schedule();
    },
    highlight(actorId) {
      // Set per rail rather than once on the layer: CSS cannot compare one
      // element's attribute against another's, and the alternative is a
      // generated stylesheet per actor.
      layer.querySelectorAll<HTMLElement>(".rail").forEach((rail) => {
        rail.toggleAttribute("data-lit", !!actorId && rail.dataset.actor === actorId);
        rail.toggleAttribute("data-dim", !!actorId && rail.dataset.actor !== actorId);
      });
    },
    destroy() {
      if (frame !== null) cancelAnimationFrame(frame);
      if (timer !== null) clearTimeout(timer);
      editor.off("update", schedule);
      window.removeEventListener("resize", schedule);
      layer.remove();
    },
  };
}
