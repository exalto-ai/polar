import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installCurrentSources } from "./current-sources";
import type { DocumentLineage } from "./mcp";

function markup() {
  document.body.innerHTML = `
    <p id="current-source-status"></p>
    <button id="current-source-retry" hidden></button>
    <ul id="current-source-list" hidden></ul>
    <p id="current-source-note" hidden></p>
  `;
}

function lineage(label = "Written here"): DocumentLineage {
  return {
    doc_id: "doc-a",
    summary: {
      total_graphemes: 12,
      total_non_whitespace_graphemes: 10,
      contributions: [],
      grouped_contributions: [{
        group: {
          key: "local:written",
          label,
          ingress: "entered",
          assurance: "observed",
          alignment: "exact",
        },
        event_count: 1,
        graphemes: 12,
        non_whitespace_graphemes: 10,
      }],
    },
    spans: [],
  };
}

describe("current sources", () => {
  beforeEach(markup);
  afterEach(() => vi.useRealTimers());

  it("shows source labels without exposing counts as percentages", async () => {
    const load = vi.fn(async () => lineage());
    const view = installCurrentSources(document, load);
    view.setDocument("doc-a");
    await vi.waitFor(() => expect(load).toHaveBeenCalledWith("doc-a"));

    const text = document.body.textContent ?? "";
    expect(text).toContain("Written here");
    expect(text).toContain("observed · exact");
    expect(text).not.toContain("12");
    expect(text).not.toContain("%");
  });

  it("ignores a response for a document that is no longer open", async () => {
    let finishFirst!: (value: DocumentLineage) => void;
    const first = new Promise<DocumentLineage>((resolve) => {
      finishFirst = resolve;
    });
    const load = vi.fn((id: string) =>
      id === "doc-a" ? first : Promise.resolve(lineage("Imported")),
    );
    const view = installCurrentSources(document, load);
    view.setDocument("doc-a");
    view.setDocument("doc-b");
    await vi.waitFor(() => expect(document.body.textContent).toContain("Imported"));
    finishFirst(lineage("Stale"));
    await Promise.resolve();

    expect(document.body.textContent).not.toContain("Stale");
  });

  it("debounces refreshes and offers an explicit retry", async () => {
    vi.useFakeTimers();
    const load = vi
      .fn<() => Promise<DocumentLineage>>()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue(lineage());
    const view = installCurrentSources(document, load);
    view.setDocument("doc-a");
    await vi.runAllTicks();
    expect(document.querySelector<HTMLButtonElement>("#current-source-retry")!.hidden).toBe(false);

    document.querySelector<HTMLButtonElement>("#current-source-retry")!.click();
    await vi.runAllTicks();
    expect(document.body.textContent).toContain("Written here");

    view.scheduleRefresh();
    view.scheduleRefresh();
    await vi.advanceTimersByTimeAsync(400);
    expect(load).toHaveBeenCalledTimes(3);
  });
});
