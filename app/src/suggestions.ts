import type { Editor } from "@tiptap/core";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";
import type * as Y from "yjs";
import { alignBlocks, blockIdOf } from "./provenance";

export type SuggestionPosition =
  | { kind: "start" }
  | { kind: "end" }
  | { kind: "block"; block_id: string };

export type SuggestionState = "pending" | "accepted" | "rejected" | "stale";

export type SuggestionNode = {
  type: string;
  content?: SuggestionNode[];
  text?: string;
};

export type SuggestionPatch =
  | { kind: "replace_block"; block_id: string; nodes: SuggestionNode[] }
  | { kind: "replace_text"; block_id: string; nodes: SuggestionNode[] }
  | {
    kind: "insert_blocks";
    after: { kind: "start" } | { kind: "end" } | { kind: "block"; block_id: string };
    nodes: SuggestionNode[];
  }
  | { kind: "delete_block"; block_id: string };

export type SuggestionRecord = {
  version: number;
  suggestion_id: string;
  document_id: string;
  request_id: string;
  proposer: {
    actor_id: string;
    connection_id: string;
    label: string;
    source_label: string;
    reported_model: string | null;
    session_id: string | null;
  };
  base_content_revision: string;
  patch: SuggestionPatch;
  explanation: string | null;
  state: SuggestionState;
  decision: { actor_id: string; actor_label: string; decided_at: number } | null;
  created_at: number;
};

export type SuggestionList = {
  content_revision: string;
  suggestions: SuggestionRecord[];
};

export type SuggestionDecisionOutcome = {
  content_revision: string;
  suggestion: SuggestionRecord;
};

export type SuggestionClient = {
  listSuggestions(documentId: string): Promise<SuggestionList>;
  acceptSuggestion(documentId: string, suggestionId: string): Promise<SuggestionDecisionOutcome>;
  rejectSuggestion(documentId: string, suggestionId: string): Promise<SuggestionDecisionOutcome>;
};

export type SuggestionReviewController = {
  refresh(): Promise<void>;
  destroy(): void;
};

type BlockPosition = { node: ProseMirrorNode; from: number; to: number };
type DecisionKind = "accept" | "reject";

const suggestionPluginKey = new PluginKey<DecorationSet>("thoughtSuggestionReview");

function blockPositions(editor: Editor, ydoc: Y.Doc): Map<string, BlockPosition> {
  const yBlocks = ydoc
    .getXmlFragment("content")
    .toArray()
    .map((node) => ({
      id: blockIdOf(node),
      kind: (node as { nodeName?: string }).nodeName ?? null,
    }));
  const editorBlocks: Array<{ node: ProseMirrorNode; offset: number }> = [];
  const kinds: string[] = [];
  editor.state.doc.forEach((node, offset) => {
    editorBlocks.push({ node, offset });
    kinds.push(node.type.name);
  });
  const ids = alignBlocks(yBlocks, kinds);
  if (!ids) return new Map();
  return new Map(
    ids.flatMap((id, index): Array<[string, BlockPosition]> => {
      const block = editorBlocks[index];
      return id && block
        ? [[id, { node: block.node, from: block.offset, to: block.offset + block.node.nodeSize }]]
        : [];
    }),
  );
}

/** Place an inserted suggestion at the current block boundary. */
export function suggestionPositionAtSelection(
  editor: Editor,
  ydoc: Y.Doc,
): SuggestionPosition {
  if (editor.state.doc.childCount === 0) return { kind: "start" };
  const positions = [...blockPositions(editor, ydoc).entries()]
    .map(([id, position]) => ({ id, ...position }))
    .sort((left, right) => left.from - right.from);
  if (positions.length === 0) {
    throw new Error("This document is still aligning with its saved version.");
  }
  const cursor = editor.state.selection.from;
  if (cursor <= positions[0].from + 1) return { kind: "start" };
  const current = positions.find(({ from, to }) => cursor >= from && cursor <= to);
  if (current) return { kind: "block", block_id: current.id };
  const previous = [...positions].reverse().find(({ to }) => to < cursor);
  return previous
    ? { kind: "block", block_id: previous.id }
    : { kind: "end" };
}

function nodeText(node: SuggestionNode): string {
  if (node.text !== undefined) return node.text;
  return (node.content ?? []).map(nodeText).join("");
}

export function proposedText(patch: SuggestionPatch): string {
  if (patch.kind === "delete_block") return "";
  return patch.nodes.map(nodeText).join("\n");
}

export function suggestionTarget(patch: SuggestionPatch): string | null {
  if (patch.kind === "insert_blocks") {
    return patch.after.kind === "block" ? patch.after.block_id : null;
  }
  return patch.block_id;
}

function anchorFor(
  suggestion: SuggestionRecord,
  positions: Map<string, BlockPosition>,
  document: ProseMirrorNode,
): number {
  const patch = suggestion.patch;
  if (patch.kind === "insert_blocks") {
    if (patch.after.kind === "start") return 0;
    if (patch.after.kind === "end") return document.content.size;
    return positions.get(patch.after.block_id)?.to ?? document.content.size;
  }
  return positions.get(patch.block_id)?.to ?? document.content.size;
}

function oneLine(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.replace(/[\r\n]+/g, " ").trim() || "The suggestion could not be reviewed.";
}

export function installSuggestionReview(
  editor: Editor,
  ydoc: Y.Doc,
  documentId: string,
  client: SuggestionClient,
  options: {
    beforeDecision?: () => Promise<boolean>;
    onNotice?: (message: string, kind?: "info" | "error") => void;
  } = {},
): SuggestionReviewController {
  const root = ydoc.getMap("suggestions");
  const content = ydoc.getXmlFragment("content");
  const suggestions = new Map<string, SuggestionRecord>();
  const busy = new Set<string>();
  const errors = new Map<string, string>();
  let destroyed = false;
  let generation = 0;
  let renderVersion = 0;
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;
  let refreshRequiresSave = false;

  const plugin = new Plugin<DecorationSet>({
    key: suggestionPluginKey,
    state: {
      init: () => DecorationSet.empty,
      apply(transaction, current) {
        const replacement = transaction.getMeta(suggestionPluginKey) as DecorationSet | undefined;
        if (replacement) return replacement;
        return transaction.docChanged ? current.map(transaction.mapping, transaction.doc) : current;
      },
    },
    props: {
      decorations(state) {
        return suggestionPluginKey.getState(state) ?? DecorationSet.empty;
      },
    },
  });

  function actionButton(
    suggestion: SuggestionRecord,
    kind: DecisionKind,
  ): HTMLButtonElement {
    const button = document.createElement("button");
    button.type = "button";
    button.className = kind === "accept" ? "suggestion-accept" : "suggestion-reject";
    button.disabled = busy.has(suggestion.suggestion_id);
    button.textContent = busy.has(suggestion.suggestion_id)
      ? "Saving…"
      : kind === "accept" ? "Accept" : "Reject";
    button.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      void decide(suggestion, kind);
    });
    return button;
  }

  function preview(suggestion: SuggestionRecord, current: string): HTMLElement {
    const container = document.createElement("div");
    container.className = "suggestion-preview";
    const proposed = proposedText(suggestion.patch);
    if (suggestion.patch.kind === "insert_blocks") {
      const insertion = document.createElement("ins");
      insertion.textContent = proposed || "Empty block";
      container.append(insertion);
      return container;
    }
    if (suggestion.patch.kind === "delete_block") {
      const deletion = document.createElement("del");
      deletion.textContent = current || "Empty block";
      container.append(deletion);
      return container;
    }
    if (current === proposed) {
      container.textContent = "Formatting or structure change";
      return container;
    }
    const deletion = document.createElement("del");
    deletion.textContent = current || "Empty block";
    const arrow = document.createElement("span");
    arrow.className = "suggestion-preview-arrow";
    arrow.textContent = "→";
    arrow.setAttribute("aria-hidden", "true");
    const insertion = document.createElement("ins");
    insertion.textContent = proposed || "Empty block";
    container.append(deletion, arrow, insertion);
    return container;
  }

  function card(
    suggestion: SuggestionRecord,
    positions: Map<string, BlockPosition>,
  ): HTMLElement {
    const card = document.createElement("aside");
    card.className = "suggestion-slip";
    card.dataset.state = suggestion.state;
    card.dataset.suggestionId = suggestion.suggestion_id;
    card.setAttribute("contenteditable", "false");
    card.setAttribute("aria-label", `Suggestion from ${suggestion.proposer.label}`);

    const heading = document.createElement("header");
    const identity = document.createElement("div");
    const eyebrow = document.createElement("span");
    eyebrow.className = "suggestion-eyebrow";
    eyebrow.textContent = suggestion.state === "stale" ? "Needs a new review" : "Reviewer's note";
    const name = document.createElement("strong");
    name.textContent = suggestion.proposer.label;
    identity.append(eyebrow, name);
    const state = document.createElement("span");
    state.className = "suggestion-state";
    state.textContent = suggestion.state === "stale" ? "Document changed" : "Pending";
    heading.append(identity, state);
    card.append(heading);

    if (suggestion.explanation) {
      const explanation = document.createElement("p");
      explanation.className = "suggestion-explanation";
      explanation.textContent = suggestion.explanation;
      card.append(explanation);
    }

    const target = suggestionTarget(suggestion.patch);
    const current = target ? positions.get(target)?.node.textContent ?? "" : "";
    card.append(preview(suggestion, current));

    if (suggestion.state === "stale") {
      const stale = document.createElement("p");
      stale.className = "suggestion-stale-note";
      stale.textContent = "The document changed after this was proposed. Ask the reviewer to try again.";
      card.append(stale);
    }

    const error = errors.get(suggestion.suggestion_id);
    if (error) {
      const alert = document.createElement("p");
      alert.className = "suggestion-error";
      alert.setAttribute("role", "alert");
      alert.textContent = error;
      card.append(alert);
    }

    const actions = document.createElement("div");
    actions.className = "suggestion-actions";
    if (suggestion.state === "pending") actions.append(actionButton(suggestion, "accept"));
    actions.append(actionButton(suggestion, "reject"));
    card.append(actions);
    return card;
  }

  function render(): void {
    if (destroyed || editor.isDestroyed) return;
    const positions = blockPositions(editor, ydoc);
    const decorations: Decoration[] = [];
    const visible = [...suggestions.values()].filter(
      ({ state }) => state === "pending" || state === "stale",
    );
    for (const [index, suggestion] of visible.entries()) {
      const target = suggestionTarget(suggestion.patch);
      const targetPosition = target ? positions.get(target) : undefined;
      if (targetPosition && suggestion.state === "pending") {
        decorations.push(
          Decoration.node(targetPosition.from, targetPosition.to, {
            class: "suggestion-target",
            "data-suggestion-id": suggestion.suggestion_id,
          }),
        );
      }
      decorations.push(
        Decoration.widget(
          anchorFor(suggestion, positions, editor.state.doc),
          () => card(suggestion, positions),
          {
            key: `suggestion-${suggestion.suggestion_id}-${suggestion.state}-${renderVersion}`,
            side: 20 + index,
            stopEvent: () => true,
            ignoreSelection: true,
          },
        ),
      );
    }
    renderVersion += 1;
    editor.view.dispatch(
      editor.state.tr
        .setMeta(suggestionPluginKey, DecorationSet.create(editor.state.doc, decorations))
        .setMeta("addToHistory", false),
    );
  }

  async function decide(suggestion: SuggestionRecord, kind: DecisionKind): Promise<void> {
    if (busy.has(suggestion.suggestion_id)) return;
    busy.add(suggestion.suggestion_id);
    errors.delete(suggestion.suggestion_id);
    render();
    try {
      if (options.beforeDecision && !(await options.beforeDecision())) {
        throw new Error("Wait for this document to finish saving, then try again.");
      }
      const outcome = kind === "accept"
        ? await client.acceptSuggestion(documentId, suggestion.suggestion_id)
        : await client.rejectSuggestion(documentId, suggestion.suggestion_id);
      if (destroyed) return;
      suggestions.set(outcome.suggestion.suggestion_id, outcome.suggestion);
      options.onNotice?.(
        kind === "accept"
          ? `Accepted ${suggestion.proposer.label}'s suggestion.`
          : `Rejected ${suggestion.proposer.label}'s suggestion.`,
      );
      editor.commands.focus();
    } catch (error) {
      if (!destroyed) {
        const message = oneLine(error);
        errors.set(suggestion.suggestion_id, message);
        options.onNotice?.(message, "error");
      }
    } finally {
      busy.delete(suggestion.suggestion_id);
      render();
    }
  }

  async function refresh(): Promise<void> {
    const currentGeneration = ++generation;
    try {
      const response = await client.listSuggestions(documentId);
      if (destroyed || currentGeneration !== generation) return;
      suggestions.clear();
      for (const suggestion of response.suggestions) {
        if (suggestion.document_id === documentId) {
          suggestions.set(suggestion.suggestion_id, suggestion);
        }
      }
      render();
    } catch (error) {
      if (!destroyed && currentGeneration === generation) {
        options.onNotice?.(`Could not load suggestions: ${oneLine(error)}`, "error");
      }
    }
  }

  function scheduleRefresh(requireSave = false): void {
    refreshRequiresSave ||= requireSave;
    if (refreshTimer !== null) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      const mustWait = refreshRequiresSave;
      refreshRequiresSave = false;
      void (async () => {
        if (mustWait && options.beforeDecision && !(await options.beforeDecision())) return;
        await refresh();
      })();
    }, 180);
  }

  function contentChanged(): void {
    let changed = false;
    for (const [id, suggestion] of suggestions) {
      if (suggestion.state === "pending") {
        suggestions.set(id, { ...suggestion, state: "stale" });
        changed = true;
      }
    }
    if (changed) render();
    if (suggestions.size > 0) scheduleRefresh(true);
  }

  editor.registerPlugin(plugin);
  const suggestionsChanged = () => scheduleRefresh();
  root.observe(suggestionsChanged);
  content.observeDeep(contentChanged);
  void refresh();

  return {
    refresh,
    destroy() {
      if (destroyed) return;
      destroyed = true;
      generation += 1;
      root.unobserve(suggestionsChanged);
      content.unobserveDeep(contentChanged);
      if (refreshTimer !== null) clearTimeout(refreshTimer);
      editor.unregisterPlugin(suggestionPluginKey);
      suggestions.clear();
      busy.clear();
      errors.clear();
    },
  };
}
