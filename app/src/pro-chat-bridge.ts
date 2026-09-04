import { invoke } from "@tauri-apps/api/core";
import type { ProProvider } from "./pro-provider-bridge";

export type ProviderModel = { id: string; display_name: string };
export type ProviderModels = { provider: ProProvider; models: ProviderModel[] };
export type ChatMessage = { role: "user" | "assistant"; text: string };
export type ThinkingLevel = "provider_default" | "low" | "medium" | "high";
export type ChatAttachment = {
  name: string;
  media_type: "application/pdf" | "text/plain";
  content_base64: string;
};

export type SendChatRequest = {
  document_title: string;
  document: unknown;
  provider: ProProvider;
  model: string;
  thinking: ThinkingLevel;
  messages: ChatMessage[];
  message: string;
  focus_text: string | null;
  attachments: ChatAttachment[];
  disclosure_version: 2;
};

export type SendChatResponse = {
  text: string;
  provider: ProProvider;
  requested_model: string;
  reported_model: string | null;
  wording_revision: string;
  complete: boolean;
};

export type ProChatBridge = {
  models(provider: ProProvider): Promise<ProviderModels>;
  send(request: SendChatRequest): Promise<SendChatResponse>;
};

export function tauriProChatBridge(): ProChatBridge {
  return {
    models: (provider) => invoke<ProviderModels>("provider_models", { provider }),
    send: (request) => invoke<SendChatResponse>("send_provider_chat", { request }),
  };
}
