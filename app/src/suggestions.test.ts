import { Editor } from "@tiptap/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as Y from "yjs";
import { extensions } from "./schema";
import {
  installSuggestionReview,
  proposedText,
  suggestionTarget,
  type SuggestionClient,
  type SuggestionRecord,
} from "./suggestions";

const editors: Editor[] = [];

function suggestion(overrides: Partial<SuggestionRecord> = {}): SuggestionRecord {
  return {
    version: 1,
    suggestion_id: "reviewer-one:request-one",
    document_id: "doc-one",
    request_id: "request-one",
    proposer: {
      actor_id: "reviewer:reviewer-one",
      connection_id: "reviewer-one",
      label: "Writing coach",
      source_label: "Configured for Codex (reported)",
      reported_model: null,
      session_id: null,
    },
    base_content_revision: "revision",
    patch: {
      kind: "replace_text",
      block_id: "1:0",
      nodes: [{ type: "paragraph", content: [{ type: "text", text: "Final" }] }],
    },
    explanation: "Use firmer wording",
    state: "pending",
    decision: null,
    created_at: 1,
    ...overrides,
  };
}

function editor(): Editor {
  const element = document.createElement("div");
  document.body.append(element);
  const value = new Editor({ element, extensions, content: "<p>Draft</p>" });
  editors.push(value);
  return value;
}

function client(record: SuggestionRecord): SuggestionClient {
  return {
    listSuggestions: vi.fn(async () => ({
      content_revision: "revision",
      suggestions: [record],
    })),
    acceptSuggestion: vi.fn(async () => ({
      content_revision: "accepted",
      suggestion: { ...record, state: "accepted" as const },
    })),
    rejectSuggestion: vi.fn(async () => ({
      content_revision: "revision",
      suggestion: { ...record, state: "rejected" as const },
    })),
  };
}

afterEach(() => {
  for (const value of editors.splice(0)) value.destroy();
  document.body.replaceChildren();
});

describe("suggestion previews", () => {
  it("extracts the proposed wording and stable target from normalized patches", () => {
    const record = suggestion();
    expect(proposedText(record.patch)).toBe("Final");
    expect(suggestionTarget(record.patch)).toBe("1:0");
    expect(suggestionTarget({
      kind: "insert_blocks",
      after: { kind: "end" },
      nodes: [],
    })).toBeNull();
  });
});

describe("suggestion review slips", () => {
  it("shows a pending proposal and removes it after acceptance", async () => {
    const record = suggestion();
    const api = client(record);
    const value = editor();
    const controller = installSuggestionReview(value, new Y.Doc(), "doc-one", api, {
      beforeDecision: vi.fn(async () => true),
    });

    await vi.waitFor(() => {
      expect(document.querySelector(".suggestion-slip")?.textContent).toContain("Writing coach");
      expect(document.querySelector(".suggestion-preview")?.textContent).toContain("Final");
    });
    document.querySelector<HTMLButtonElement>(".suggestion-accept")!.click();
    await vi.waitFor(() => {
      expect(api.acceptSuggestion).toHaveBeenCalledWith(
        "doc-one",
        "reviewer-one:request-one",
      );
      expect(document.querySelector(".suggestion-slip")).toBeNull();
    });
    controller.destroy();
  });

  it("explains stale proposals and offers rejection but not acceptance", async () => {
    const record = suggestion({ state: "stale" });
    const api = client(record);
    const value = editor();
    const controller = installSuggestionReview(value, new Y.Doc(), "doc-one", api);

    await vi.waitFor(() => {
      expect(document.querySelector(".suggestion-stale-note")?.textContent).toContain(
        "Ask the reviewer to try again",
      );
    });
    expect(document.querySelector(".suggestion-accept")).toBeNull();
    expect(document.querySelector(".suggestion-reject")).not.toBeNull();
    controller.destroy();
  });
});
