/**
 * The document schema — the single source of truth (M2.2).
 *
 * TipTap builds its schema from Extensions and cannot load a raw ProseMirror
 * spec, so this file defines the shape and `scripts/export-schema.ts` writes
 * the structural half to `crates/thought-schema/schema.json` for Rust to read.
 * CI fails if the committed JSON drifts from what these extensions produce.
 *
 * Drift is worth that much ceremony because of how it fails: agents start
 * producing documents the editor rejects, and the symptom shows up far from
 * the cause, looking like a CRDT bug.
 */
import StarterKit from "@tiptap/starter-kit";
import { Table, TableRow, TableCell, TableHeader } from "@tiptap/extension-table";
import { FontSize } from "./font-size";
import { TitleVariant } from "./title";

export const extensions = [
  StarterKit.configure({
    // Yjs owns history; StarterKit's undo would fight it and undo other
    // people's edits (AD-11).
    undoRedo: false,
    // Not in the v0 schema: markdown renders a hard break as a space, so one
    // could not survive the projection (AD-12).
    hardBreak: false,
    // Likewise absent from CommonMark and GFM.
    underline: false,
    heading: { levels: [1, 2, 3] },
    // A click selects the marked text so our contextual link card can offer
    // explicit open, copy, edit, and remove actions. Opening immediately makes
    // a link impossible to edit with the mouse.
    link: { openOnClick: false, enableClickSelection: true },
  }),
  TitleVariant,
  FontSize,
  Table.configure({ resizable: true }),
  TableRow,
  TableHeader,
  TableCell,
];
