import { describe, it, expect, vi } from "vitest";
import * as Y from "yjs";
import {
  alignBlocks,
  blockIdOf,
  installProvenanceRails,
  labelFor,
  runsOf,
} from "./provenance";
import type { BlockAttribution } from "./mcp";

function attribution(over: Partial<BlockAttribution> = {}): BlockAttribution {
  return {
    block_id: "1:0",
    created_by: "agent:opus",
    created_at: Date.now(),
    touched_by: "agent:opus",
    touched_at: Date.now(),
    session_id: "run-1",
    kind: "agent",
    display_name: "opus",
    model: "claude-opus-5",
    color: "#4c8dff",
    ...over,
  };
}

describe("block ids", () => {
  /**
   * The format is `polar_core::block::block_id`'s, and the two must agree or
   * every rail silently misses. Pinned here because the Rust side is pinned by
   * its own tests and nothing else compares them.
   */
  it("spells a block id the way the daemon does", () => {
    const doc = new Y.Doc();
    const fragment = doc.getXmlFragment("content");
    fragment.insert(0, [new Y.XmlElement("paragraph")]);

    const block = fragment.toArray()[0];
    const id = blockIdOf(block);
    expect(id).toMatch(/^\d+:\d+$/);
    expect(id).toBe(`${doc.clientID}:0`);
  });

  it("gives no id for something that is not in a document", () => {
    expect(blockIdOf(new Y.XmlElement("paragraph"))).toBeNull();
    expect(blockIdOf(undefined)).toBeNull();
  });

  it("keeps a block's id when its contents change", () => {
    const doc = new Y.Doc();
    const fragment = doc.getXmlFragment("content");
    fragment.insert(0, [new Y.XmlElement("paragraph")]);
    const block = fragment.toArray()[0] as Y.XmlElement;
    const before = blockIdOf(block);

    block.insert(0, [new Y.XmlText("some words")]);
    expect(blockIdOf(block)).toBe(before);
  });
});

describe("aligning the CRDT with the editor", () => {
  const y = [
    { id: "1:0", kind: "heading" },
    { id: "1:4", kind: "paragraph" },
  ];

  /** TipTap keeps a trailing paragraph that never enters the CRDT, so the
   *  editor is legitimately longer and a length comparison rejects everything. */
  it("ignores the editor's trailing paragraph", () => {
    expect(alignBlocks(y, ["heading", "paragraph", "paragraph"])).toEqual(["1:0", "1:4"]);
  });

  it("lines up when the two agree exactly", () => {
    expect(alignBlocks(y, ["heading", "paragraph"])).toEqual(["1:0", "1:4"]);
  });

  /** The case worth refusing: a rail on the wrong paragraph is a lie, so a
   *  disagreement skips the frame rather than guessing. */
  it("refuses when the kinds disagree at a position", () => {
    expect(alignBlocks(y, ["paragraph", "heading", "paragraph"])).toBeNull();
  });

  it("refuses when the editor has fewer blocks than the CRDT", () => {
    expect(alignBlocks(y, ["heading"])).toBeNull();
  });

  it("draws nothing rather than refusing for an empty document", () => {
    expect(alignBlocks([], ["paragraph"])).toEqual([]);
  });
});

describe("runs", () => {
  it("draws adjacent blocks by one actor as a single bar", () => {
    const runs = runsOf(["agent:opus", "agent:opus", "human:editor"]);
    expect(runs).toEqual([
      { actorId: "agent:opus", from: 0, to: 1 },
      { actorId: "human:editor", from: 2, to: 2 },
    ]);
  });

  it("does not join blocks that are not adjacent", () => {
    const runs = runsOf(["agent:opus", "human:editor", "agent:opus"]);
    expect(runs).toHaveLength(3);
  });

  /** Unattributed is not the same as yours, so it breaks a run rather than
   *  being absorbed into one. */
  it("leaves unattributed blocks undrawn and breaks the run there", () => {
    const runs = runsOf(["agent:opus", null, "agent:opus"]);
    expect(runs).toEqual([
      { actorId: "agent:opus", from: 0, to: 0 },
      { actorId: "agent:opus", from: 2, to: 2 },
    ]);
  });

  it("draws nothing for a document nobody is attributed in", () => {
    expect(runsOf([null, null])).toEqual([]);
  });
});

describe("labels", () => {
  it("names the actor and its model", () => {
    const label = labelFor(attribution(), "human:editor");
    expect(label).toContain("opus");
    expect(label).toContain("claude-opus-5");
  });

  it("says 'You' rather than the user's own actor id", () => {
    const mine = attribution({ touched_by: "human:editor", created_by: "human:editor" });
    expect(labelFor(mine, "human:editor")).toMatch(/^You · /);
  });

  /** The whole reason `created_by` is stored separately: saying only "You"
   *  about a paragraph an agent drafted erases where the words came from. */
  it("keeps who drafted a block the user later reworded", () => {
    const reworded = attribution({
      created_by: "agent:opus",
      touched_by: "human:editor",
      display_name: "editor",
      kind: "human",
      model: null,
    });
    expect(labelFor(reworded, "human:editor")).toBe(
      `You · just now, drafted by opus`,
    );
  });

  it("does not claim a draft credit when one actor did both", () => {
    expect(labelFor(attribution(), "human:editor")).not.toContain("drafted by");
  });
});

/**
 * A real Y.Doc, so `blockIdOf` is exercised rather than mocked, plus the
 * smallest editor the rails actually touch: a DOM child per block and a way to
 * enumerate node kinds.
 */
function harness(kinds: string[]) {
  const ydoc = new Y.Doc();
  const fragment = ydoc.getXmlFragment("content");
  ydoc.transact(() => {
    for (const kind of kinds) fragment.push([new Y.XmlElement(kind)]);
  });

  const container = document.createElement("div");
  const dom = document.createElement("div");
  container.append(dom);
  for (const _ of kinds) dom.append(document.createElement("p"));

  const editor = {
    view: { dom },
    state: {
      doc: {
        forEach: (visit: (node: { type: { name: string } }) => void) =>
          kinds.forEach((kind) => visit({ type: { name: kind } })),
      },
    },
    on() {},
    off() {},
  } as never;

  const ids = fragment.toArray().map((node) => blockIdOf(node)!);
  return { editor, ydoc, container, ids };
}

describe("redrawing when nothing is painted", () => {
  it("draws even though requestAnimationFrame never fires", async () => {
    // A hidden window paints no frames. A frame-only schedule leaves the margin
    // crediting whoever wrote a block last week while an agent rewrites it, and
    // corrects itself only when someone looks at the window.
    const raf = vi.spyOn(globalThis, "requestAnimationFrame").mockImplementation(() => 1);
    try {
      const { editor, ydoc, container, ids } = harness(["paragraph", "paragraph"]);
      const rails = installProvenanceRails(editor, ydoc, container, "human:me");

      rails.setProvenance([
        attribution({ block_id: ids[0], touched_by: "human:me", kind: "human" }),
        attribution({ block_id: ids[1], touched_by: "agent:opus" }),
      ]);

      expect(container.querySelectorAll(".rail")).toHaveLength(0);
      await new Promise((resolve) => setTimeout(resolve, 120));
      expect(container.querySelectorAll(".rail").length).toBeGreaterThan(0);

      rails.destroy();
    } finally {
      raf.mockRestore();
    }
  });

  it("draws nothing for a document only this window has written", async () => {
    const { editor, ydoc, container, ids } = harness(["paragraph"]);
    const rails = installProvenanceRails(editor, ydoc, container, "human:me");

    rails.setProvenance([
      attribution({ block_id: ids[0], touched_by: "human:me", kind: "human" }),
    ]);
    await new Promise((resolve) => setTimeout(resolve, 120));

    // Striping a solo document tells the writer something they already know.
    expect(container.querySelectorAll(".rail")).toHaveLength(0);
    rails.destroy();
  });
});
