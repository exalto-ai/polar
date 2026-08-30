import type { DocumentView } from "./mcp";

export class EditorDocuments {
  constructor(
    private readonly mcpUrl: string,
    private readonly token: string,
  ) {}

  createDocument(title: string, initialMarkdown?: string): Promise<DocumentView> {
    return this.post("/editor/documents", {
      title,
      ...(initialMarkdown === undefined ? {} : { markdown: initialMarkdown }),
    });
  }

  setDocumentDeleted(docId: string, deleted: boolean): Promise<unknown> {
    return this.post(`/editor/documents/${encodeURIComponent(docId)}/deletion`, { deleted });
  }

  private async post(path: string, body: unknown): Promise<any> {
    const response = await fetch(new URL(path, this.mcpUrl), {
      method: "POST",
      headers: {
        Authorization: `Bearer ${this.token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });
    if (!response.ok) {
      throw new Error((await response.text()) || `editor request failed (${response.status})`);
    }
    return response.json();
  }
}
