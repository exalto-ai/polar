import { describe, expect, it } from "vitest";
import { colorFor, playfulName, seedFrom } from "./names";

describe("playful names", () => {
  it("is stable for the same peer", () => {
    // You learn who is who across a session only if the name does not move.
    expect(playfulName(12345)).toBe(playfulName(12345));
    expect(colorFor(12345)).toBe(colorFor(12345));
  });

  it("separates neighbouring client ids", () => {
    // Yjs client ids are not adjacent in practice, but an index that used the
    // raw seed would put 1 and 2 on adjacent names and look broken when they
    // were.
    const names = [1, 2, 3, 4, 5].map(playfulName);
    expect(new Set(names).size).toBe(5);
  });

  it("varies both halves, not just the adjective", () => {
    const creatures = new Set(
      Array.from({ length: 200 }, (_, i) => playfulName(i * 7919).split(" ")[1]),
    );
    expect(creatures.size).toBeGreaterThan(10);
  });

  it("rarely collides at the scale that actually happens", () => {
    // A document has a handful of windows open, not hundreds. Drawing 300
    // names from 400 combinations gives ~211 distinct by the birthday problem,
    // so asserting more than that would be testing luck rather than the
    // generator. Test the real case instead: small groups, many times over.
    let clean = 0;
    const groups = 400;
    for (let g = 0; g < groups; g++) {
      const group = Array.from({ length: 6 }, (_, i) => playfulName(g * 104729 + i * 7919));
      if (new Set(group).size === 6) clean += 1;
    }
    expect(clean / groups).toBeGreaterThan(0.9);
  });

  it("derives a seed from a string for actors named by id", () => {
    expect(seedFrom("agent:opus")).toBe(seedFrom("agent:opus"));
    expect(seedFrom("agent:opus")).not.toBe(seedFrom("agent:sonnet"));
  });
});
