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
    return "In the ChatGPT desktop app, open Settings → MCP servers → Add server. Enter the server name below, choose STDIO, paste the command, save, then select Restart. In the composer, type /mcp to verify the connection. ChatGPT web does not read this local configuration.";
  }
  if (client === "codex") {
    return "Run the command below once in Terminal, then use /mcp in Codex to check the connection. The ChatGPT desktop app on this Mac uses the same MCP configuration.";
  }
  if (client === "claude-code") {
    return "Run the command below once in Terminal, then use /mcp in Claude Code to check the connection.";
  }
  return "In Claude Desktop, open Developer settings and edit claude_desktop_config.json. Merge the server entry below into mcpServers, save the file, then fully quit and reopen Claude Desktop. To check it, click + in a chat → Connectors, or look in Developer settings.";
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
  if (client === "claude-code") {
    return `claude mcp add --transport stdio --scope user ${server} -- ${invocation}`;
  }
  return JSON.stringify(
    {
      mcpServers: {
        [server]: {
          command: executable,
          args: ["--connection", id],
        },
      },
    },
    null,
    2,
  );
}

export function reviewerSetupCopyLabel(client: ReviewerClient): string {
  return client === "claude-desktop" ? "Copy JSON" : "Copy command";
}

export function reviewerSetupServerName(connectionId: string): string | null {
  const id = connectionId.trim();
  return /^[a-z0-9-]{1,64}$/.test(id) ? `thought-${id}` : null;
}

function shellArgument(value: string): string {
  if (/^[A-Za-z0-9_./:@%+=,-]+$/.test(value)) return value;
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}
