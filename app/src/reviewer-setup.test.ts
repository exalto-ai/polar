import { describe, expect, it } from "vitest";
import {
  reviewerSetupCommand,
  reviewerSetupInstructions,
} from "./reviewer-setup";

describe("reviewer setup", () => {
  it("passes only the stable connection id to the stdio shim", () => {
    expect(
      reviewerSetupCommand("codex", "/Applications/Proof of Thought/shim", "abc123"),
    ).toBe(
      "codex mcp add thought-abc123 -- '/Applications/Proof of Thought/shim' --connection abc123",
    );
  });

  it("rejects malformed connection ids", () => {
    expect(reviewerSetupCommand("chatgpt", "shim", "../secret")).toBeNull();
  });

  it("explains where ChatGPT desktop accepts its stdio command", () => {
    const instructions = reviewerSetupInstructions("chatgpt");
    expect(instructions).toContain("Settings → MCP servers → Add server");
    expect(instructions).toContain("Choose STDIO");
    expect(instructions).toContain("ChatGPT on the web does not use this local setup");
  });
});
