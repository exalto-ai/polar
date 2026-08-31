export type ReviewerClient =
  | "chatgpt"
  | "codex"
  | "claude-desktop"
  | "claude-code";

const names: Record<ReviewerClient, string> = {
  chatgpt: "ChatGPT desktop",
  codex: "Codex",
  "claude-desktop": "Claude Desktop",
  "claude-code": "Claude Code",
};

export function reviewerClientName(client: ReviewerClient): string {
  return names[client];
}

export function reviewerSetupInstructions(client: ReviewerClient): string {
  if (client === "chatgpt") {
    return "In ChatGPT desktop, open Settings → MCP servers → Add server. Choose STDIO, paste the command below, save, then restart. ChatGPT on the web does not use this local setup.";
  }
  if (client === "codex") {
    return "Run the command below once in Terminal, then use /mcp in Codex to check the connection.";
  }
  if (client === "claude-code") {
    return "Run the command below once in Terminal, then use /mcp in Claude Code to check the connection.";
  }
  return "Claude Desktop setup is unavailable in this build.";
}

/** The setup text contains a connection ID, never its credential. */
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
    !/^[a-z0-9-]{1,64}$/.test(id) ||
    /[\u0000\r\n]/.test(executable)
  ) {
    return null;
  }
  const invocation = `${shellArgument(executable)} --connection ${id}`;
  const server = `thought-${id}`;
  if (client === "chatgpt") return invocation;
  if (client === "codex") return `codex mcp add ${server} -- ${invocation}`;
  return `claude mcp add --scope user ${server} -- ${invocation}`;
}

function shellArgument(value: string): string {
  if (/^[A-Za-z0-9_./:@%+=,-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}
