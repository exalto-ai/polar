import { describe, expect, it } from "vitest";
import markup from "../index.html?raw";

describe("AI support product claims", () => {
  it("discloses persistent per-reviewer access and provider transfer", () => {
    expect(markup).toContain("Each saved route has its own Proof of Thought access");
    expect(markup).not.toContain("persistent whole-workspace access");
    expect(markup).toContain("This document");
    expect(markup).toContain("All documents");
    expect(markup).toContain("Reviewer connections are read-only");
    expect(markup).not.toContain("Edit directly");
    expect(markup).not.toContain("Move to Trash");
    expect(markup).toContain("even when no Proof of Thought window is open");
    expect(markup).toContain("may send that content to an AI provider");
    expect(markup).toContain("immediately blocks new reads");
    expect(markup).toContain("not other Mac access the tool already has");
  });

  it("keeps Basic attribution conservative", () => {
    expect(markup).toContain("Unclassified change");
    expect(markup).toContain("Exact attribution is shown only when anchored evidence supports it");
    expect(markup).toContain("Proof of Thought makes no AI request");
    expect(markup).toContain("remain active and listed above");
  });

  it("does not retain the raw connection bypass or overstate Pro evidence", () => {
    expect(markup).not.toContain('id="copy-command"');
    expect(markup).not.toContain("Point any MCP client");
    expect(markup).not.toContain("provider-verified");
    expect(markup).toContain(
      "bind a provider-authenticated exchange to the accepted change",
    );
  });

  it("labels configured routes without claiming that an app made the call", () => {
    expect(markup).toContain("The app name shows how you configured the read-only route");
    expect(markup).toContain("does not prove which app used the route");
    expect(markup).toContain('value="claude-desktop" disabled');
    expect(markup).not.toContain("provider verified reviewer");
    expect(markup).not.toContain("app and model names are reported by that client");
  });
});
