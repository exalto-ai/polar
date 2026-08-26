/**
 * The editor: TipTap over the shared Y.Doc, with the composition guard wired
 * to the provider.
 */
import {
  combineTransactionSteps,
  Editor,
  Extension,
  getChangedRanges,
} from "@tiptap/core";
import Collaboration from "@tiptap/extension-collaboration";
import CollaborationCaret from "@tiptap/extension-collaboration-caret";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";
import type { Transaction } from "@tiptap/pm/state";
import type { Transform } from "@tiptap/pm/transform";
import type { Awareness } from "y-protocols/awareness";
import type * as Y from "yjs";
import { extensions } from "./schema";
import { syncEditorReadiness } from "./editor-lifecycle";
import type { SyncProvider } from "./provider";
import { installLinkShortcut } from "./link";
import { installSlashMenu } from "./slash";
import { installToolbar, type ToolbarOptions } from "./toolbar";
import {
  LocalInputSource,
  MAX_ANCHORED_HINTS,
  type AnchoredRangeHint,
  type LocalInputSource as LocalInputSourceValue,
} from "./protocol";

export type Actor = { name: string; color: string; id: number };
export type EditorActions = Omit<ToolbarOptions, "openLink" | "subscribeSaveStatus">;

/** App commands may set this on their TipTap transaction explicitly. */
export const INPUT_SOURCE_META = "thought.inputSource";

type GraphemeSegment = { index: number; segment: string };
type GraphemeSegmenter = {
  segment: (text: string) => Iterable<GraphemeSegment>;
};
type GraphemeSegmenterConstructor = new (
  locales?: string | string[],
  options?: { granularity: "grapheme" },
) => GraphemeSegmenter;

type PositionedGrapheme = { from: number; to: number };

function isLocalInputSource(value: unknown): value is LocalInputSourceValue {
  return Object.values(LocalInputSource).includes(value as LocalInputSourceValue);
}

/**
 * Prefer transaction facts over the surrounding DOM event. Actual paste and
 * cut transactions are tagged by ProseMirror itself; an explicit app command
 * is next. Everything else needs an observed editor event or remains unknown.
 */
export function transactionInputSource(
  transaction: Transaction,
  observed: LocalInputSourceValue = LocalInputSource.Unknown,
): LocalInputSourceValue {
  const uiEvent = transaction.getMeta("uiEvent");
  if (uiEvent === "paste" || transaction.getMeta("paste") === true) {
    return LocalInputSource.Paste;
  }
  if (uiEvent === "cut") return LocalInputSource.Command;
  if (uiEvent === "drop") return LocalInputSource.Unknown;

  const explicit = transaction.getMeta(INPUT_SOURCE_META);
  if (isLocalInputSource(explicit)) return explicit;
  if (transaction.getMeta("composition") !== undefined) {
    return LocalInputSource.Written;
  }
  return observed;
}

function positionedGraphemes(doc: ProseMirrorNode): PositionedGrapheme[] | null {
  const Segmenter = (
    Intl as unknown as { Segmenter?: GraphemeSegmenterConstructor }
  ).Segmenter;
  if (!Segmenter) return null;

  const segmenter = new Segmenter(undefined, { granularity: "grapheme" });
  const graphemes: PositionedGrapheme[] = [];
  doc.descendants((node, position) => {
    if (!node.isText || !node.text) return;
    for (const item of segmenter.segment(node.text)) {
      graphemes.push({
        from: position + item.index,
        to: position + item.index + item.segment.length,
      });
    }
  });
  return graphemes;
}

function snapToGraphemeBoundary(
  position: number,
  graphemes: readonly PositionedGrapheme[],
  edge: "from" | "to",
): number {
  let low = 0;
  let high = graphemes.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    const grapheme = graphemes[middle];
    if (position <= grapheme.from) {
      high = middle;
    } else if (position >= grapheme.to) {
      low = middle + 1;
    } else {
      return edge === "from" ? grapheme.from : grapheme.to;
    }
  }
  return position;
}

function includeGraphemeBefore(
  position: number,
  graphemes: readonly PositionedGrapheme[],
): number {
  let low = 0;
  let high = graphemes.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (graphemes[middle].to < position) low = middle + 1;
    else high = middle;
  }
  return graphemes[low]?.to === position ? graphemes[low].from : position;
}

function includeGraphemeAfter(
  position: number,
  graphemes: readonly PositionedGrapheme[],
): number {
  let low = 0;
  let high = graphemes.length;
  while (low < high) {
    const middle = Math.floor((low + high) / 2);
    if (graphemes[middle].from < position) low = middle + 1;
    else high = middle;
  }
  return graphemes[low]?.from === position ? graphemes[low].to : position;
}

function shouldMergeHints(
  left: AnchoredRangeHint,
  right: AnchoredRangeHint,
): boolean {
  const overlaps =
    right.beforeFrom < left.beforeTo || right.afterFrom < left.afterTo;
  const touchesInBoth =
    right.beforeFrom === left.beforeTo && right.afterFrom === left.afterTo;
  return overlaps || touchesInBoth;
}

function mergeHints(
  left: AnchoredRangeHint,
  right: AnchoredRangeHint,
): AnchoredRangeHint {
  return {
    beforeFrom: Math.min(left.beforeFrom, right.beforeFrom),
    beforeTo: Math.max(left.beforeTo, right.beforeTo),
    afterFrom: Math.min(left.afterFrom, right.afterFrom),
    afterTo: Math.max(left.afterTo, right.afterTo),
  };
}

/**
 * Capture ProseMirror's positions on both sides of a transform. Positions that
 * land inside an extended Unicode grapheme are expanded out to its boundaries.
 * The final ranges are sorted and overlapping ranges are merged in both
 * coordinate spaces, which is the canonical form accepted by the daemon.
 * Incomplete or unsupported evidence falls back to zero hints and V1 inference.
 */
export function transactionAnchorHints(
  transaction: Transform,
): AnchoredRangeHint[] {
  if (transaction.steps.length === 0) return [];
  const changed = getChangedRanges(transaction);
  const beforeGraphemes = positionedGraphemes(transaction.before);
  const afterGraphemes = positionedGraphemes(transaction.doc);
  if (!beforeGraphemes || !afterGraphemes) return [];

  const beforeSize = transaction.before.content.size;
  const afterSize = transaction.doc.content.size;
  const normalized: AnchoredRangeHint[] = [];
  for (const { oldRange, newRange } of changed) {
    if (
      oldRange.from < 0 ||
      oldRange.from > oldRange.to ||
      oldRange.to > beforeSize ||
      newRange.from < 0 ||
      newRange.from > newRange.to ||
      newRange.to > afterSize
    ) {
      return [];
    }
    let beforeFrom = snapToGraphemeBoundary(
      oldRange.from,
      beforeGraphemes,
      "from",
    );
    let beforeTo = snapToGraphemeBoundary(
      oldRange.to,
      beforeGraphemes,
      "to",
    );
    let afterFrom = snapToGraphemeBoundary(
      newRange.from,
      afterGraphemes,
      "from",
    );
    let afterTo = snapToGraphemeBoundary(
      newRange.to,
      afterGraphemes,
      "to",
    );

    // A combining mark can turn an insertion at a formerly safe boundary into
    // part of the adjacent grapheme. Expand the corresponding side too, or the
    // daemon would see unequal text outside the two anchor ranges and reject
    // otherwise valid evidence.
    if (beforeFrom < oldRange.from || afterFrom < newRange.from) {
      beforeFrom = includeGraphemeBefore(beforeFrom, beforeGraphemes);
      afterFrom = includeGraphemeBefore(afterFrom, afterGraphemes);
    }
    if (beforeTo > oldRange.to || afterTo > newRange.to) {
      beforeTo = includeGraphemeAfter(beforeTo, beforeGraphemes);
      afterTo = includeGraphemeAfter(afterTo, afterGraphemes);
    }

    normalized.push({ beforeFrom, beforeTo, afterFrom, afterTo });
  }

  normalized.sort(
    (left, right) =>
      left.beforeFrom - right.beforeFrom ||
      left.beforeTo - right.beforeTo ||
      left.afterFrom - right.afterFrom ||
      left.afterTo - right.afterTo,
  );

  const canonical: AnchoredRangeHint[] = [];
  for (const hint of normalized) {
    let current = hint;
    while (
      canonical.length > 0 &&
      shouldMergeHints(canonical[canonical.length - 1], current)
    ) {
      current = mergeHints(canonical.pop()!, current);
    }
    canonical.push(current);
  }
  if (canonical.length > MAX_ANCHORED_HINTS) return [];
  return canonical;
}

class InputSourceTracker {
  private observed: LocalInputSourceValue = LocalInputSource.Unknown;
  private generation = 0;

  constructor(private readonly provider: SyncProvider) {}

  note(source: LocalInputSourceValue) {
    this.observed = source;
    this.provider.noteLocalInputSource(source);
    const generation = ++this.generation;
    window.setTimeout(() => {
      if (this.generation === generation) {
        this.observed = LocalInputSource.Unknown;
      }
    }, 0);
  }

  sourceFor(transaction: Transaction): LocalInputSourceValue {
    return transactionInputSource(transaction, this.observed);
  }

  destroy() {
    this.generation += 1;
    this.observed = LocalInputSource.Unknown;
  }
}

function inputSourceExtension(
  provider: SyncProvider,
  tracker: InputSourceTracker,
): Extension {
  return Extension.create({
    name: "thoughtInputSource",
    // Wrap the complete dispatch chain, including transactions appended by
    // input and paste rules, while the final document is written into Yjs.
    priority: 10_000,
    dispatchTransaction({ transaction, next }) {
      const hints = transactionAnchorHints(transaction);
      let appendedTransactions: Transaction[] | null = null;
      const onTransaction = (event: {
        transaction: Transaction;
        appendedTransactions: Transaction[];
      }) => {
        if (event.transaction === transaction) {
          appendedTransactions = event.appendedTransactions;
        }
      };
      this.editor.on("transaction", onTransaction);
      try {
        provider.withLocalTransaction(tracker.sourceFor(transaction), hints, () => {
          next(transaction);
          if (appendedTransactions === null) {
            // A downstream dispatcher that applies work without reporting the
            // complete chain cannot safely attach root-only evidence.
            hints.splice(0);
          } else if (appendedTransactions.length > 0) {
            const complete = combineTransactionSteps(transaction.before, [
              transaction,
              ...appendedTransactions,
            ]);
            hints.splice(0, hints.length, ...transactionAnchorHints(complete));
          }
        });
      } finally {
        this.editor.off("transaction", onTransaction);
      }
    },
  });
}

function sourceForBeforeInput(event: InputEvent): LocalInputSourceValue {
  const inputType = event.inputType;
  if (inputType.startsWith("insertFromPaste") || inputType === "insertFromYank") {
    return LocalInputSource.Paste;
  }
  if (
    inputType.startsWith("format") ||
    inputType.startsWith("history") ||
    inputType === "deleteByCut"
  ) {
    return LocalInputSource.Command;
  }
  // A drop can be an external insertion or a move of existing document text.
  // Until those are distinguished, guessing either paste or written would be
  // a stronger claim than the event supports.
  if (inputType === "insertFromDrop" || inputType === "deleteByDrag") {
    return LocalInputSource.Unknown;
  }
  if (inputType.startsWith("insert") || inputType.startsWith("delete")) {
    return LocalInputSource.Written;
  }
  return LocalInputSource.Unknown;
}

/** Observe the browser interaction that caused the next editor dispatch. */
function installInputSourceTracking(
  editor: Editor,
  host: HTMLElement,
  tracker: InputSourceTracker,
): () => void {
  const dom = editor.view.dom;
  const commandSurface = ".format-toolbar, .link-card, .slash";

  const onBeforeInput = (event: Event) => {
    if (event instanceof InputEvent) tracker.note(sourceForBeforeInput(event));
  };
  const onPaste = () => tracker.note(LocalInputSource.Paste);
  const onCut = () => tracker.note(LocalInputSource.Command);
  const onDrop = () => tracker.note(LocalInputSource.Unknown);
  const onCompositionStart = () => tracker.note(LocalInputSource.Written);
  const onEditorKeyDown = (event: KeyboardEvent) => {
    const slashOpen = host.querySelector<HTMLElement>(".slash:not([hidden])") !== null;
    if (slashOpen && event.key === "Enter") {
      tracker.note(LocalInputSource.Command);
      return;
    }
    const accelerator =
      (event.metaKey || event.ctrlKey) && !event.getModifierState("AltGraph");
    if (accelerator) {
      tracker.note(LocalInputSource.Command);
    } else if (
      event.key.length === 1 ||
      ["Enter", "Backspace", "Delete", "Tab"].includes(event.key)
    ) {
      tracker.note(LocalInputSource.Written);
    }
  };
  const onCommandSurface = (event: Event) => {
    const target = event.target;
    if (target instanceof Element && target.closest(commandSurface)) {
      tracker.note(LocalInputSource.Command);
    }
  };

  dom.addEventListener("beforeinput", onBeforeInput, true);
  dom.addEventListener("paste", onPaste, true);
  dom.addEventListener("cut", onCut, true);
  dom.addEventListener("drop", onDrop, true);
  dom.addEventListener("compositionstart", onCompositionStart, true);
  dom.addEventListener("keydown", onEditorKeyDown, true);
  host.addEventListener("click", onCommandSurface, true);
  host.addEventListener("change", onCommandSurface, true);
  host.addEventListener("keydown", onCommandSurface, true);

  return () => {
    dom.removeEventListener("beforeinput", onBeforeInput, true);
    dom.removeEventListener("paste", onPaste, true);
    dom.removeEventListener("cut", onCut, true);
    dom.removeEventListener("drop", onDrop, true);
    dom.removeEventListener("compositionstart", onCompositionStart, true);
    dom.removeEventListener("keydown", onEditorKeyDown, true);
    host.removeEventListener("click", onCommandSurface, true);
    host.removeEventListener("change", onCommandSurface, true);
    host.removeEventListener("keydown", onCommandSurface, true);
    tracker.destroy();
  };
}

/**
 * The caret each peer leaves behind.
 *
 * The default renders a label that is always visible, which turns a busy
 * document into a wall of name tags. This one carries the peer's id so the
 * presence chips can point at a specific caret, and keeps the label hidden
 * until you ask for it — by hovering the caret, or the chip that names it.
 */
function renderCaret(user: Actor): HTMLElement {
  const caret = document.createElement("span");
  caret.className = "peer-caret";
  caret.style.setProperty("--who", user.color);
  caret.dataset.peer = String(user.id);

  const label = document.createElement("span");
  label.className = "peer-label";
  label.textContent = user.name;
  caret.append(label);
  return caret;
}

export function createEditor(
  host: HTMLElement,
  element: HTMLElement,
  doc: Y.Doc,
  awareness: Awareness,
  provider: SyncProvider,
  user: Actor,
  actions: EditorActions,
  shouldAutoFocus: () => boolean = () => true,
): Editor {
  const sourceTracker = new InputSourceTracker(provider);
  const editor = new Editor({
    element,
    extensions: [
      inputSourceExtension(provider, sourceTracker),
      ...extensions,
      // The fragment name must match the daemon's root (thought_core::CONTENT).
      Collaboration.configure({ fragment: doc.getXmlFragment("content") }),
      CollaborationCaret.configure({
        provider: { awareness } as never,
        user,
        render: renderCaret as never,
      }),
    ],
    editable: provider.isHydrated,
    autofocus: provider.isHydrated && shouldAutoFocus() ? "end" : false,
  });

  const readiness = { editor, provider };
  const unsubscribeHydration = provider.subscribeHydration((hydrated) => {
    syncEditorReadiness(readiness);
    if (hydrated && editor.isEditable && shouldAutoFocus()) {
      editor.commands.focus("end");
    }
  });

  // Install before command surfaces so capture listeners can label the
  // synchronous transaction they cause.
  const destroySourceTracking = installInputSourceTracking(editor, host, sourceTracker);
  const destroySlashMenu = installSlashMenu(editor, host);
  const links = installLinkShortcut(editor, host);
  const destroyToolbar = installToolbar(editor, element, {
    ...actions,
    openLink: links.open,
    subscribeSaveStatus: (listener) => provider.subscribeSaveStatus(listener),
  });

  // Menus and toolbar controls live outside ProseMirror's element, so TipTap
  // cannot remove them when a document switch destroys the editor.
  editor.on("destroy", () => {
    unsubscribeHydration();
    destroySourceTracking();
    destroySlashMenu();
    links.destroy();
    destroyToolbar();
  });

  // AD-17. y-prosemirror applies remote updates while an input method has live
  // marked text, redrawing the node being composed in. The provider holds them
  // until the composition commits.
  const dom = editor.view.dom;
  dom.addEventListener("compositionstart", () => provider.setComposing(true));
  dom.addEventListener("compositionend", () => provider.setComposing(false));

  return editor;
}
