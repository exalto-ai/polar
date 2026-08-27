import { Channel, invoke } from "@tauri-apps/api/core";

export type ProChatProvider = "openai" | "anthropic";

export type ProChatThinking =
  | "default"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max";

export type ProChatTurnStatus =
  | "pending"
  | "completed"
  | "stopped"
  | "failed"
  | "interrupted"
  | "incomplete";

export type ProChatErrorCategory =
  | "invalid_request"
  | "authentication"
  | "permission"
  | "billing"
  | "spend_or_usage_limit"
  | "rate_limited"
  | "model_unavailable"
  | "provider_unavailable"
  | "timeout"
  | "network_or_tls_failure"
  | "invalid_provider_response"
  | "conversation_changed"
  | "storage"
  | "refusal";

export type ProChatModel = {
  id: string;
  display_name: string;
  thinking_levels: ProChatThinking[];
};

export type ProChatProviderCapability = {
  provider: ProChatProvider;
  display_name: string;
  status: string;
  models: ProChatModel[];
};

export type ProChatCapabilities = {
  providers: ProChatProviderCapability[];
};

export type ProChatTurn = {
  id: string;
  user_text: string;
  assistant_text: string;
  status: ProChatTurnStatus;
  provider: ProChatProvider;
  requested_model: string;
  reported_model: string | null;
  thinking: ProChatThinking;
  created_at: number;
  completed_at: number | null;
  request_id: string | null;
  error_category: ProChatErrorCategory | null;
  retryable: boolean;
  input_tokens: number | null;
  output_tokens: number | null;
};

export type ProChatHistory = {
  document_id: string;
  provider: ProChatProvider;
  revision: number;
  turns: ProChatTurn[];
};

export type ProChatStartRequest = {
  document_id: string;
  provider: ProChatProvider;
  expected_revision: number;
  model: string;
  thinking: ProChatThinking;
  message: string | null;
  retry_turn_id: string | null;
  disclosure_version: number;
};

export type ProChatStartResult = {
  operation_id: string;
  turn_id: string;
};

export type ProChatEvent =
  | {
      type: "started";
      operation_id: string;
      turn: ProChatTurn;
      revision: number;
    }
  | {
      type: "delta";
      operation_id: string;
      turn_id: string;
      text: string;
    }
  | {
      type: "completed" | "stopped" | "failed";
      operation_id: string;
      turn: ProChatTurn;
      revision: number;
      error_message?: string | null;
    };

export type ProChatBridge = {
  capabilities(): Promise<ProChatCapabilities>;
  history(
    documentId: string,
    provider: ProChatProvider,
  ): Promise<ProChatHistory>;
  start(
    request: ProChatStartRequest,
    onEvent: (event: ProChatEvent) => void,
  ): Promise<ProChatStartResult>;
  stop(operationId: string): Promise<boolean>;
  clear(
    documentId: string,
    provider: ProChatProvider,
    expectedRevision: number,
  ): Promise<ProChatHistory>;
};

export const PRO_CHAT_DISCLOSURE_VERSION = 1;

/**
 * Provider credentials stay native. The webview supplies only the conversation
 * people can see, their selected controls, and a versioned disclosure.
 */
export function tauriProChatBridge(): ProChatBridge {
  return {
    capabilities: () => invoke<ProChatCapabilities>("pro_chat_capabilities"),
    history: (documentId, provider) =>
      invoke<ProChatHistory>("pro_chat_history", { documentId, provider }),
    start: (request, onEvent) => {
      const channel = new Channel<ProChatEvent>();
      channel.onmessage = onEvent;
      return invoke<ProChatStartResult>("start_pro_chat", {
        request,
        onEvent: channel,
      });
    },
    stop: (operationId) =>
      invoke<boolean>("stop_pro_chat", { operationId }),
    clear: (documentId, provider, expectedRevision) =>
      invoke<ProChatHistory>("clear_pro_chat", {
        documentId,
        provider,
        expectedRevision,
      }),
  };
}
