/**
 * The editor: TipTap over the shared Y.Doc, with the composition guard wired
 * to the provider.
 */
import { Editor, Extension, getChangedRanges } from "@tiptap/core";
import Collaboration from "@tiptap/extension-collaboration";
import CollaborationCaret from "@tiptap/extension-collaboration-caret";
import type { Awareness } from "y-protocols/awareness";
import type * as Y from "yjs";
import type { Transform } from "@tiptap/pm/transform";
import { extensions } from "./schema";
import type { SyncProvider } from "./provider";
import { installLinkShortcut } from "./link";
import { installSlashMenu } from "./slash";
import { installToolbar, type ToolbarOptions } from "./toolbar";
import { MAX_EDITOR_RANGES, type EditorRange } from "./protocol";

export type Actor = { name: string; color: string; id: number };
export type EditorActions = Omit<ToolbarOptions, "openLink" | "subscribeSaveStatus">;

export function transactionRanges(transaction: Transform): EditorRange[] {
  if (transaction.steps.length === 0) return [];
  const ranges = getChangedRanges(transaction).map(({ oldRange, newRange }) => ({
    beforeFrom: oldRange.from,
    beforeTo: oldRange.to,
    afterFrom: newRange.from,
    afterTo: newRange.to,
  }));
  return ranges.length <= MAX_EDITOR_RANGES ? ranges : [];
}

function editorMutationExtension(provider: SyncProvider): Extension {
  return Extension.create({
    name: "thoughtEditorMutation",
    priority: 10_000,
    dispatchTransaction({ transaction, next }) {
      provider.withEditorTransaction(transactionRanges(transaction), () => next(transaction));
    },
  });
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

/**
 * Treat the quiet writing canvas as part of the editor, not as dead space.
 *
 * ProseMirror only handles a press whose target is inside its document DOM.
 * Short documents leave most of the blue page outside any text block, so a
 * press there otherwise keeps focus in the toolbar or AI sidebar. The three
 * direct background targets below deliberately exclude real blocks, toolbar
 * controls, suggestion cards, and provenance rails, which retain their own
 * selection and activation behavior.
 */
export function installEditorCanvasFocus(editor: Editor, element: HTMLElement): () => void {
  const canvas = element.closest<HTMLElement>(".page") ?? element;
  const editorDom = editor.view.dom;
  const backgroundTargets = new Set<EventTarget>([canvas, element, editorDom]);
  const focusAtEnd = (event: MouseEvent) => {
    if (
      event.button !== 0 ||
      !editor.isEditable ||
      event.target === null ||
      !backgroundTargets.has(event.target)
    ) return;

    event.preventDefault();
    editor.commands.focus("end");
    // TipTap's command may schedule the DOM focus for the next frame. A canvas
    // press must be ready for the very next keystroke, including in a window
    // that was focused in the AI sidebar immediately beforehand.
    editor.view.focus();
  };

  canvas.addEventListener("mousedown", focusAtEnd);
  return () => canvas.removeEventListener("mousedown", focusAtEnd);
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
  const editor = new Editor({
    element,
    extensions: [
      editorMutationExtension(provider),
      ...extensions,
      // The fragment name must match the daemon's root (thought_core::CONTENT).
      Collaboration.configure({ fragment: doc.getXmlFragment("content") }),
      CollaborationCaret.configure({
        provider: { awareness } as never,
        user,
        render: renderCaret as never,
      }),
    ],
    autofocus: false,
    editable: false,
  });

  const unsubscribeHydration = provider.subscribeHydration((hydrated) => {
    editor.setEditable(hydrated, false);
    if (hydrated && shouldAutoFocus()) editor.commands.focus("end");
  });

  const destroyCanvasFocus = installEditorCanvasFocus(editor, element);
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
    destroySlashMenu();
    destroyCanvasFocus();
    links.destroy();
    destroyToolbar();
    unsubscribeHydration();
  });

  // AD-17. y-prosemirror applies remote updates while an input method has live
  // marked text, redrawing the node being composed in. The provider holds them
  // until the composition commits.
  const dom = editor.view.dom;
  dom.addEventListener("compositionstart", () => provider.setComposing(true));
  dom.addEventListener("compositionend", () => provider.setComposing(false));

  return editor;
}
