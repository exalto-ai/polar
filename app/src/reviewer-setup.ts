export type ReviewerClient =
  | "chatgpt"
  | "codex"
  | "claude-desktop"
  | "claude-code";

export type ReviewerProvider = "openai" | "anthropic";

export type ReviewerClientDefinition = {
  id: ReviewerClient;
  name: string;
  shortName: string;
  provider: ReviewerProvider;
  availability: "available" | "planned";
  setup: string;
  caveat: string | null;
};

export const REVIEWER_CLIENTS: readonly ReviewerClientDefinition[] = [
  {
    id: "chatgpt",
    name: "ChatGPT desktop",
    shortName: "ChatGPT",
    provider: "openai",
    availability: "available",
    setup:
      "Open ChatGPT settings, choose MCP servers, then add a STDIO server. Paste the command below and restart ChatGPT.",
    caveat: "ChatGPT on the web cannot reach this local editor.",
  },
  {
    id: "codex",
    name: "Codex",
    shortName: "Codex",
    provider: "openai",
    availability: "available",
    setup: "Copy the command below into Terminal once, then start a new Codex session.",
    caveat: "This uses the Codex command already installed on your Mac.",
  },
  {
    id: "claude-desktop",
    name: "Claude Desktop",
    shortName: "Claude",
    provider: "anthropic",
    availability: "planned",
    setup: "Claude Desktop needs a packaged local extension before it can be connected safely.",
    caveat: "Use Claude Code for now. The Claude Desktop option will unlock when the extension ships.",
  },
  {
    id: "claude-code",
    name: "Claude Code",
    shortName: "Claude Code",
    provider: "anthropic",
    availability: "available",
    setup:
      "Copy the command below into Terminal once, then use /mcp in Claude Code to check the connection.",
    caveat: "This uses the Claude command already installed on your Mac.",
  },
] as const;

export function reviewerClient(client: ReviewerClient): ReviewerClientDefinition {
  const definition = REVIEWER_CLIENTS.find(({ id }) => id === client);
  if (!definition) throw new Error(`Unknown reviewer client: ${String(client)}`);
  return definition;
}

/**
 * Produce text a person may choose to copy. This function never launches a
 * client or shell. The stable connection ID is the only connection identity
 * included in the command, and no bearer credential crosses into the webview.
 */
export function reviewerSetupCommand(
  client: ReviewerClient,
  stdioExecutable: string,
  connectionId: string,
): string | null {
  const executable = stdioExecutable.trim();
  const id = connectionId.trim();
  if (
    client === "claude-desktop" ||
    !executable ||
    !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(id) ||
    /[\u0000\r\n]/.test(executable)
  ) {
    return null;
  }

  const invocation = `${shellArgument(executable)} --connection ${shellArgument(id)}`;
  if (client === "chatgpt") return invocation;

  const serverName = `thought-${safeServerSuffix(id)}`;
  if (client === "codex") {
    return `codex mcp add ${serverName} -- ${invocation}`;
  }
  return `claude mcp add --scope user ${serverName} -- ${invocation}`;
}

function safeServerSuffix(connectionId: string): string {
  const safe = connectionId.replace(/[^A-Za-z0-9_-]/g, "").slice(0, 64);
  return safe || "reviewer";
}

function shellArgument(value: string): string {
  if (/^[A-Za-z0-9_./:@%+=,-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}
