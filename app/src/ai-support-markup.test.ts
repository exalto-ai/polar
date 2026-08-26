import { describe, expect, it } from "vitest";
import markup from "../index.html?raw";

describe("AI support product claims", () => {
  it("discloses the current whole-workspace connection access", () => {
    expect(markup).toContain("Current access");
    expect(markup).toContain(
      "temporary shared setup lets the AI app read, search, create, edit, and move any document in this local Proof of Thought workspace to Trash",
    );
    expect(markup).toContain("even when no Proof of Thought window is open");
    expect(markup).toContain("it may send that content to its AI provider");
    expect(markup).toContain("remove Proof of Thought from that AI app");
  });

  it("keeps Basic attribution conservative", () => {
    expect(markup).toContain("Unclassified change");
    expect(markup).toContain("Exact attribution is shown only when anchored evidence supports it");
    expect(markup).toContain("Proof of Thought makes no AI request");
  });

  it("does not retain the raw connection bypass or overstate Pro evidence", () => {
    expect(markup).not.toContain('id="copy-command"');
    expect(markup).not.toContain("Point any MCP client");
    expect(markup).not.toContain("provider-verified");
    expect(markup).toContain(
      "bind a provider-authenticated exchange to the accepted change",
    );
  });
});
