import type { DocumentView } from "./mcp";
import type {
  DirectEditAccess,
  DirectEditApi,
  DirectEditDenial,
  DirectEditGrant,
} from "./direct-edit-access";
import type { ProProvider } from "./pro-provider-bridge";
import type {
  ReviewerApi,
  ReviewerConnection,
  ReviewerInput,
} from "./reviewer-connections";
import type {
  SuggestionDecisionOutcome,
  SuggestionList,
  SuggestionPosition,
} from "./suggestions";

export type ChatSuggestionInput = {
  documentId: string;
  requestId: string;
  provider: ProProvider;
  requestedModel: string;
  reportedModel: string | null;
  assistantText: string;
  wordingRevision: string;
  after: SuggestionPosition;
};

export class EditorApi implements ReviewerApi, DirectEditApi {
  private readonly baseUrl: string;

  constructor(
    mcpUrl: string,
    private readonly token: string,
    private readonly fetcher: typeof fetch = globalThis.fetch.bind(globalThis),
  ) {
    this.baseUrl = new URL(mcpUrl).origin;
  }

  createDocument(title: string, initialMarkdown?: string): Promise<DocumentView> {
    return this.request("POST", "/editor/documents", {
      title,
      ...(initialMarkdown === undefined ? {} : { markdown: initialMarkdown }),
    });
  }

  setDocumentDeleted(docId: string, deleted: boolean): Promise<unknown> {
    return this.request("POST", `/editor/documents/${encodeURIComponent(docId)}/deletion`, {
      deleted,
    });
  }

  listSuggestions(docId: string): Promise<SuggestionList> {
    return this.request(
      "GET",
      `/editor/documents/${encodeURIComponent(docId)}/suggestions`,
    );
  }

  proposeChatSuggestion(input: ChatSuggestionInput): Promise<SuggestionDecisionOutcome> {
    return this.request(
      "POST",
      `/editor/documents/${encodeURIComponent(input.documentId)}/suggestions/pro-chat`,
      {
        request_id: input.requestId,
        provider: input.provider,
        requested_model: input.requestedModel,
        reported_model: input.reportedModel,
        assistant_text: input.assistantText,
        wording_revision: input.wordingRevision,
        after: input.after,
      },
    );
  }

  acceptSuggestion(
    docId: string,
    suggestionId: string,
  ): Promise<SuggestionDecisionOutcome> {
    return this.request(
      "POST",
      `/editor/documents/${encodeURIComponent(docId)}/suggestions/${encodeURIComponent(suggestionId)}/accept`,
    );
  }

  rejectSuggestion(
    docId: string,
    suggestionId: string,
  ): Promise<SuggestionDecisionOutcome> {
    return this.request(
      "POST",
      `/editor/documents/${encodeURIComponent(docId)}/suggestions/${encodeURIComponent(suggestionId)}/reject`,
    );
  }

  listDirectEditAccess(): Promise<DirectEditAccess> {
    return this.request("GET", "/editor/direct-edit-access");
  }

  approveDirectEdit(documentId: string, requestId: string): Promise<DirectEditGrant> {
    return this.item(
      "grant",
      this.request(
        "POST",
        `/editor/documents/${encodeURIComponent(documentId)}/direct-edit-requests/${encodeURIComponent(requestId)}/approve`,
      ),
    );
  }

  denyDirectEdit(documentId: string, requestId: string): Promise<DirectEditDenial> {
    return this.item(
      "denial",
      this.request(
        "POST",
        `/editor/documents/${encodeURIComponent(documentId)}/direct-edit-requests/${encodeURIComponent(requestId)}/deny`,
      ),
    );
  }

  revokeDirectEdit(documentId: string, grantId: string): Promise<DirectEditGrant> {
    return this.item(
      "grant",
      this.request(
        "DELETE",
        `/editor/documents/${encodeURIComponent(documentId)}/direct-edit-grants/${encodeURIComponent(grantId)}`,
      ),
    );
  }

  async listReviewerConnections(): Promise<ReviewerConnection[]> {
    const value = await this.request<{ connections: ReviewerConnection[] }>(
      "GET",
      "/editor/reviewer-connections",
    );
    return value.connections;
  }

  createReviewerConnection(input: ReviewerInput): Promise<ReviewerConnection> {
    return this.connection(
      this.request("POST", "/editor/reviewer-connections", input),
    );
  }

  updateReviewerConnection(
    id: string,
    input: Omit<ReviewerInput, "client"> & { expected_revision: number },
  ): Promise<ReviewerConnection> {
    return this.connection(
      this.request(
        "PATCH",
        `/editor/reviewer-connections/${encodeURIComponent(id)}`,
        input,
      ),
    );
  }

  resetReviewerConnection(id: string, expectedRevision: number): Promise<ReviewerConnection> {
    return this.connection(
      this.request(
        "POST",
        `/editor/reviewer-connections/${encodeURIComponent(id)}/reset`,
        { expected_revision: expectedRevision },
      ),
    );
  }

  revokeReviewerConnection(id: string, expectedRevision: number): Promise<ReviewerConnection> {
    return this.connection(
      this.request(
        "DELETE",
        `/editor/reviewer-connections/${encodeURIComponent(id)}`,
        { expected_revision: expectedRevision },
      ),
    );
  }

  private async connection(
    request: Promise<{ connection: ReviewerConnection }>,
  ): Promise<ReviewerConnection> {
    return (await request).connection;
  }

  private async item<T>(key: string, request: Promise<Record<string, T>>): Promise<T> {
    return (await request)[key];
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await this.fetcher(new URL(path, this.baseUrl), {
      method,
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const raw = await response.text();
    let value: unknown = raw;
    try {
      value = raw ? JSON.parse(raw) : null;
    } catch {
      // Plain-text errors from Axum are already suitable for the toast.
    }
    if (!response.ok) {
      throw new Error(
        typeof value === "string"
          ? value
          : ((value as { error?: string } | null)?.error ??
              `editor request failed (${response.status})`),
      );
    }
    return value as T;
  }
}

/** Temporary source compatibility for callers that only use document methods. */
export { EditorApi as EditorDocuments };
