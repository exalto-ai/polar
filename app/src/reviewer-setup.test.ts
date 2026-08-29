import { describe, expect, it } from "vitest";
import { REVIEWER_CLIENTS, reviewerSetupCommand } from "./reviewer-setup";

describe("reviewer setup", () => {
  const executable = "/Applications/Proof of Thought.app/Contents/MacOS/thought-mcp-stdio";

  it("uses a stable connection ID without placing a credential in setup text", () => {
    const command = reviewerSetupCommand("codex", executable, "connection-123")!;

    expect(command).toBe(
      "codex mcp add thought-connection-123 -- '/Applications/Proof of Thought.app/Contents/MacOS/thought-mcp-stdio' --connection connection-123",
    );
    expect(command).not.toMatch(/bearer|secret|token|api[-_ ]?key/i);
  });

  it("keeps same-name reviewers distinct through IDs, not display labels", () => {
    const first = reviewerSetupCommand("claude-code", executable, "connection-a");
    const second = reviewerSetupCommand("claude-code", executable, "connection-b");

    expect(first).not.toBe(second);
    expect(first).toContain("connection-a");
    expect(second).toContain("connection-b");
    expect(first).not.toContain("My Claude reviewer");
    const sharedPrefix = "reviewer-1234567890";
    expect(
      reviewerSetupCommand("codex", executable, `${sharedPrefix}-a`),
    ).not.toBe(reviewerSetupCommand("codex", executable, `${sharedPrefix}-b`));
  });

  it("quotes apostrophes and rejects unsafe structured values", () => {
    expect(
      reviewerSetupCommand(
        "chatgpt",
        "/Applications/Proof's Thought/thought-mcp-stdio",
        "connection-1",
      ),
    ).toBe(
      "'/Applications/Proof'\"'\"'s Thought/thought-mcp-stdio' --connection connection-1",
    );
    expect(reviewerSetupCommand("codex", `${executable}\nopen bad`, "connection-1")).toBeNull();
    expect(reviewerSetupCommand("codex", executable, "connection 1")).toBeNull();
  });

  it("keeps Claude Desktop unavailable until extension packaging exists", () => {
    expect(
      REVIEWER_CLIENTS.find(({ id }) => id === "claude-desktop")?.availability,
    ).toBe("planned");
    expect(reviewerSetupCommand("claude-desktop", executable, "connection-1")).toBeNull();
  });
});
