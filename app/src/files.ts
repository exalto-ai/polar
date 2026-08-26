/** Native Markdown import/export, kept behind a tiny injectable bridge. */
import { invoke } from "@tauri-apps/api/core";

export type ImportedMarkdown = {
  file_name: string;
  markdown: string;
};

export type ExportedMarkdown = { file_name: string };

export type FileBridge = {
  importMarkdown: () => Promise<ImportedMarkdown | null>;
  exportMarkdown: (
    document: unknown,
    suggestedName: string,
  ) => Promise<ExportedMarkdown | null>;
};

export const nativeFileBridge: FileBridge = {
  importMarkdown: () => invoke<ImportedMarkdown | null>("import_markdown"),
  exportMarkdown: (document, suggestedName) =>
    invoke<ExportedMarkdown | null>("export_markdown", { document, suggestedName }),
};

export type DocumentCreator = {
  createDocument: (title: string, initialMarkdown?: string) => Promise<{ doc_id: string }>;
};

/** The file name is only a fallback. Once imported, document content is authoritative. */
export function titleFromFileName(fileName: string): string {
  const leaf = fileName.split(/[\\/]/).pop() ?? "";
  return leaf.replace(/\.(?:md|markdown)$/i, "").trim();
}

export function suggestedMarkdownName(title: string): string {
  const safe = title
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "-")
    .replace(/\s+/g, " ")
    .replace(/[. ]+$/g, "")
    .trim()
    .slice(0, 100);
  return `${safe || "Untitled"}.md`;
}

/** Import creates a new CRDT document from a snapshot, never over a live one. */
export async function importMarkdownDocument(
  bridge: FileBridge,
  documents: DocumentCreator,
  openDocument: (docId: string) => Promise<boolean | void>,
): Promise<ImportedMarkdown | null> {
  const file = await bridge.importMarkdown();
  if (!file) return null;

  const created = await documents.createDocument(
    titleFromFileName(file.file_name),
    file.markdown,
  );
  const opened = await openDocument(created.doc_id);
  if (opened === false) {
    throw new Error("the current document still has changes waiting to autosave");
  }
  return file;
}

/** Export projects the current tree as a one-time snapshot at a newly chosen path. */
export async function exportMarkdownDocument(
  bridge: FileBridge,
  document: unknown,
  title: string,
): Promise<ExportedMarkdown | null> {
  return bridge.exportMarkdown(document, suggestedMarkdownName(title));
}
