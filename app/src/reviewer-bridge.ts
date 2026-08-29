import type { ReviewerClient, ReviewerProvider } from "./reviewer-setup";

export type ReviewerStatus =
  | "configured"
  | "connected"
  | "disconnected"
  | "failed"
  | "revoked";

export type ReviewerDocumentScope = "current" | "all";
export type ReviewerFailureCode =
  | "transport"
  | "protocol"
  | "credential_missing"
  | "credential_store";

export type ReviewerPermissions = {
  document_scope: ReviewerDocumentScope;
  can_read: boolean;
  can_edit: boolean;
  can_create: boolean;
  can_trash: boolean;
  document_ids: string[];
};

export type ReviewerConnection = {
  id: string;
  client: ReviewerClient;
  provider: ReviewerProvider;
  display_label: string;
  status: ReviewerStatus;
  permissions: ReviewerPermissions;
  revision: number;
  created_at: number;
  first_connected_at: number | null;
  last_seen_at: number | null;
  failure_code: ReviewerFailureCode | null;
  revoked_at: number | null;
  reported_model: string | null;
};

export type CreateReviewerConnection = {
  client: ReviewerClient;
  display_label: string;
  permissions: ReviewerPermissions;
};

export type UpdateReviewerConnection = {
  expected_revision: number;
  display_label?: string;
  permissions?: ReviewerPermissions;
};

export type ReviewerBridge = {
  list: () => Promise<ReviewerConnection[]>;
  create: (input: CreateReviewerConnection) => Promise<ReviewerConnection>;
  update: (id: string, input: UpdateReviewerConnection) => Promise<ReviewerConnection>;
  reset: (id: string, expectedRevision: number) => Promise<ReviewerConnection>;
  revoke: (id: string, expectedRevision: number) => Promise<ReviewerConnection>;
};

export type ReviewerEndpointClient = {
  listReviewerConnections: () => Promise<ReviewerConnection[]>;
  createReviewerConnection: (
    input: CreateReviewerConnection,
  ) => Promise<ReviewerConnection>;
  updateReviewerConnection: (
    id: string,
    input: UpdateReviewerConnection,
  ) => Promise<ReviewerConnection>;
  resetReviewerConnection: (
    id: string,
    expectedRevision: number,
  ) => Promise<ReviewerConnection>;
  revokeReviewerConnection: (
    id: string,
    expectedRevision: number,
  ) => Promise<ReviewerConnection>;
};

/** Keep the UI coupled to a narrow, injectable capability instead of HTTP. */
export function reviewerBridge(client: ReviewerEndpointClient): ReviewerBridge {
  return {
    list: () => client.listReviewerConnections(),
    create: (input) => client.createReviewerConnection(input),
    update: (id, input) => client.updateReviewerConnection(id, input),
    reset: (id, expectedRevision) =>
      client.resetReviewerConnection(id, expectedRevision),
    revoke: (id, expectedRevision) =>
      client.revokeReviewerConnection(id, expectedRevision),
  };
}

export function permissionsForScope(
  documentScope: ReviewerDocumentScope,
  documentId: string,
  values: Omit<ReviewerPermissions, "document_scope" | "document_ids">,
): ReviewerPermissions {
  return {
    document_scope: documentScope,
    ...values,
    document_ids: documentScope === "current" && documentId ? [documentId] : [],
  };
}
