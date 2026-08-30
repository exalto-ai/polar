import type { DocumentLineage } from "./mcp";

export type CurrentSources = {
  setDocument(docId: string | null): void;
  scheduleRefresh(): void;
  destroy(): void;
};

const REFRESH_DELAY_MS = 400;

/** Render current visible-text sources. History stays in the CRDT update log. */
export function installCurrentSources(
  root: ParentNode,
  load: (docId: string) => Promise<DocumentLineage>,
): CurrentSources {
  const list = root.querySelector<HTMLUListElement>("#current-source-list")!;
  const status = root.querySelector<HTMLElement>("#current-source-status")!;
  const note = root.querySelector<HTMLElement>("#current-source-note")!;
  const retry = root.querySelector<HTMLButtonElement>("#current-source-retry")!;

  let docId: string | null = null;
  let timer: number | null = null;
  let request = 0;

  function render(lineage: DocumentLineage) {
    const sources = lineage.summary.grouped_contributions;
    list.replaceChildren(
      ...sources.map(({ group }) => {
        const item = document.createElement("li");
        const label = document.createElement("strong");
        label.textContent = group.label;
        const detail = document.createElement("span");
        detail.textContent = [group.assurance, group.alignment]
          .map((value) => value.replace(/_/g, " "))
          .join(" · ");
        item.append(label, detail);
        return item;
      }),
    );
    list.hidden = sources.length === 0;
    status.textContent = sources.length === 0
      ? "No visible text has a recorded source yet."
      : "";
    note.hidden = sources.length === 0;
    retry.hidden = true;
  }

  async function refresh() {
    const target = docId;
    if (!target) {
      list.replaceChildren();
      list.hidden = true;
      note.hidden = true;
      retry.hidden = true;
      status.textContent = "Open a document to see its sources.";
      return;
    }
    const currentRequest = ++request;
    status.textContent = "Loading sources…";
    retry.hidden = true;
    try {
      const lineage = await load(target);
      if (currentRequest === request && target === docId) render(lineage);
    } catch {
      if (currentRequest !== request || target !== docId) return;
      status.textContent = "Could not load sources.";
      retry.hidden = false;
    }
  }

  retry.addEventListener("click", refresh);

  return {
    setDocument(next) {
      docId = next;
      void refresh();
    },
    scheduleRefresh() {
      if (!docId) return;
      if (timer !== null) clearTimeout(timer);
      timer = window.setTimeout(() => {
        timer = null;
        void refresh();
      }, REFRESH_DELAY_MS);
    },
    destroy() {
      request += 1;
      if (timer !== null) clearTimeout(timer);
      retry.removeEventListener("click", refresh);
    },
  };
}
