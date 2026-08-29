import { describe, expect, it, vi } from "vitest";
import {
  permissionsForScope,
  reviewerBridge,
  type ReviewerConnection,
  type ReviewerEndpointClient,
} from "./reviewer-bridge";

const connection: ReviewerConnection = {
  id: "connection-1",
  client: "codex",
  provider: "openai",
  display_label: "Reviewer",
  status: "configured",
  permissions: {
    document_scope: "current",
    can_read: true,
    can_edit: true,
    can_create: false,
    can_trash: false,
    document_ids: ["doc-1"],
  },
  revision: 2,
  created_at: 1,
  first_connected_at: null,
  last_seen_at: null,
  failure_code: null,
  revoked_at: null,
  reported_model: null,
};

function endpoint(): ReviewerEndpointClient {
  return {
    listReviewerConnections: vi.fn().mockResolvedValue([connection]),
    createReviewerConnection: vi.fn().mockResolvedValue(connection),
    updateReviewerConnection: vi.fn().mockResolvedValue(connection),
    resetReviewerConnection: vi.fn().mockResolvedValue(connection),
    revokeReviewerConnection: vi.fn().mockResolvedValue(connection),
  };
}

describe("reviewer bridge", () => {
  it("delegates only the narrow reviewer capability", async () => {
    const client = endpoint();
    const bridge = reviewerBridge(client);
    const create = {
      client: "codex" as const,
      display_label: "Reviewer",
      permissions: connection.permissions,
    };

    await bridge.list();
    await bridge.create(create);
    await bridge.update(connection.id, { expected_revision: 2, display_label: "New" });
    await bridge.reset(connection.id, 3);
    await bridge.revoke(connection.id, 4);

    expect(client.createReviewerConnection).toHaveBeenCalledWith(create);
    expect(client.updateReviewerConnection).toHaveBeenCalledWith(connection.id, {
      expected_revision: 2,
      display_label: "New",
    });
    expect(client.resetReviewerConnection).toHaveBeenCalledWith(connection.id, 3);
    expect(client.revokeReviewerConnection).toHaveBeenCalledWith(connection.id, 4);
    expect(JSON.stringify(create)).not.toMatch(/bearer|secret|token/i);
  });

  it("builds consistent current and all-document permission payloads", () => {
    const values = {
      can_read: true,
      can_edit: false,
      can_create: true,
      can_trash: false,
    };
    expect(permissionsForScope("current", "doc-1", values)).toEqual({
      document_scope: "current",
      ...values,
      document_ids: ["doc-1"],
    });
    expect(permissionsForScope("all", "doc-1", values)).toEqual({
      document_scope: "all",
      ...values,
      document_ids: [],
    });
  });
});
