import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const markup = readFileSync(resolve(import.meta.dirname, "../index.html"), "utf8");

describe("AI support path copy", () => {
  it("makes review, cost, and assurance differences explicit", () => {
    expect(markup).toContain("Accept and Reject");
    expect(markup).toContain("no separate API charge");
    expect(markup).toContain("Provider API charges apply");
    expect(markup).toContain("reported by the connected tool");
    expect(markup).toContain("Accepted wording is still labeled as reported AI output");
    expect(markup).toContain("does not disconnect existing reviewers");
    expect(markup).toContain("currently grouped as editor-entered");
    expect(markup).not.toContain("Authenticated exchanges support deeper traces");
  });

  it("offers the four supported local setup paths", () => {
    expect(markup).toContain("ChatGPT desktop");
    expect(markup).toContain(">Codex<");
    expect(markup).toContain("Claude Code");
    expect(markup).toContain('value="claude-desktop"');
  });

  it("explains that Last used does not imply an AI edit", () => {
    expect(markup).toContain("Last used can include a quiet connection check");
    expect(markup).toContain("It does not mean the AI edited this document");
    expect(markup).not.toContain("MCP keepalive");
  });

  it("keeps connection-lifetime direct editing visible outside every AI mode", () => {
    expect(markup.indexOf('id="direct-edit-active"')).toBeLessThan(
      markup.indexOf('class="ai-mode-switcher"'),
    );
    expect(markup).toContain("while they remain open");
    expect(markup).toContain("Direct edits do not have Accept or Reject. Suggestions still do.");
    expect(markup).toContain("Keep suggestions");
    expect(markup).toContain("Allow direct editing");
    expect(markup).not.toContain("Allow for 15 minutes");
    expect(markup).not.toContain("cleanup delay");
    expect(markup).not.toContain("inactive client must request direct access again");
  });
});
