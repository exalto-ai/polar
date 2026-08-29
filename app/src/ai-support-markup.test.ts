import { describe, expect, it } from "vitest";
import markup from "../index.html?raw";

describe("AI support product claims", () => {
  it("keeps reviewer setup unavailable until the bounded connection layer", () => {
    expect(markup).toContain("Connection preview");
    expect(markup).toContain(
      "does not provide connection setup yet",
    );
    expect(markup).toContain("bounded read-only reviewer connections");
    expect(markup).toContain("Available in the next update");
    expect(markup).not.toContain("Copy setup");
    expect(markup).not.toContain("can edit directly");
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
