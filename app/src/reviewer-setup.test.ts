import { describe, expect, it } from "vitest";
import {
  reviewerSetupCommand,
  reviewerSetupCopyLabel,
  reviewerSetupInstructions,
  reviewerSetupServerName,
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
    expect(instructions).toContain("choose STDIO");
    expect(instructions).toContain("select Restart");
    expect(instructions).toContain("ChatGPT web does not read this local configuration");
    expect(reviewerSetupCommand("chatgpt", "/path/to/shim", "abc123")).toBe(
      "/path/to/shim --connection abc123",
    );
  });

  it("uses Claude Code's explicit stdio transport", () => {
    expect(reviewerSetupCommand("claude-code", "/path/to/shim", "abc123")).toBe(
      "claude mcp add --transport stdio --scope user thought-abc123 -- /path/to/shim --connection abc123",
    );
  });

  it("provides a mergeable Claude Desktop configuration without shell quoting", () => {
    const value = reviewerSetupCommand(
      "claude-desktop",
      "/Applications/Proof of Thought/shim",
      "abc123",
    );
    expect(JSON.parse(value!)).toEqual({
      mcpServers: {
        "thought-abc123": {
          command: "/Applications/Proof of Thought/shim",
          args: ["--connection", "abc123"],
        },
      },
    });
    expect(reviewerSetupInstructions("claude-desktop")).toContain(
      "claude_desktop_config.json",
    );
    expect(reviewerSetupCopyLabel("claude-desktop")).toBe("Copy JSON");
    expect(reviewerSetupServerName("abc123")).toBe("thought-abc123");
  });

  it("JSON-escapes unusual executable paths and rejects control characters", () => {
    const executable = `/Applications/Proof ' “思考” \\ helper`;
    const value = reviewerSetupCommand("claude-desktop", executable, "abc123");
    expect(JSON.parse(value!).mcpServers["thought-abc123"].command).toBe(executable);
    expect(reviewerSetupCommand("claude-desktop", "bad\npath", "abc123")).toBeNull();
  });
});
