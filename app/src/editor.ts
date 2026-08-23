/**
 * The editor: TipTap over the shared Y.Doc, with the composition guard wired
 * to the provider.
 */
import { Editor } from "@tiptap/core";
import Collaboration from "@tiptap/extension-collaboration";
import CollaborationCaret from "@tiptap/extension-collaboration-caret";
import type { Awareness } from "y-protocols/awareness";
import type * as Y from "yjs";
import { extensions } from "./schema";
import type { SyncProvider } from "./provider";

export type Actor = { name: string; color: string };

export function createEditor(
  element: HTMLElement,
  doc: Y.Doc,
  awareness: Awareness,
  provider: SyncProvider,
  user: Actor,
): Editor {
  const editor = new Editor({
    element,
    extensions: [
      ...extensions,
      // The fragment name must match the daemon's root (polar_core::CONTENT).
      Collaboration.configure({ fragment: doc.getXmlFragment("content") }),
      CollaborationCaret.configure({ provider: { awareness } as never, user }),
    ],
    autofocus: "end",
  });

  // AD-17. y-prosemirror applies remote updates while an input method has live
  // marked text, redrawing the node being composed in. The provider holds them
  // until the composition commits.
  const dom = editor.view.dom;
  dom.addEventListener("compositionstart", () => provider.setComposing(true));
  dom.addEventListener("compositionend", () => provider.setComposing(false));

  return editor;
}
