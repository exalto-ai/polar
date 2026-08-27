import { invoke } from "@tauri-apps/api/core";

export type ProProvider = "openai" | "anthropic";

export type ProviderValidationStatus =
  | "not_checked"
  | "model_access_checked"
  | "invalid_key_format"
  | "credential_or_access_invalid"
  | "permission_denied"
  | "billing_unavailable"
  | "spend_or_usage_limit"
  | "rate_limited"
  | "unsupported_region"
  | "provider_unavailable"
  | "timeout"
  | "network_or_tls_failure"
  | "invalid_provider_response"
  | "model_unavailable"
  | "credential_missing";

export type ProviderConfiguration = {
  provider: ProProvider;
  configured: boolean;
  removal_pending: boolean;
  validation_status: ProviderValidationStatus;
  last_checked_at: number | null;
  last_validated_at: number | null;
  model_count: number | null;
  request_id: string | null;
  disclosure_version: number | null;
  charges_acknowledged_at: number | null;
};

export type ProviderActionResult = {
  outcome: "saved" | "cancelled" | "validation_failed" | "checked" | "removed";
  configuration: ProviderConfiguration;
  attempt_status: ProviderValidationStatus | null;
  request_id: string | null;
};

export type ProProviderBridge = {
  list: () => Promise<ProviderConfiguration[]>;
  configure: (provider: ProProvider) => Promise<ProviderActionResult>;
  revalidate: (provider: ProProvider) => Promise<ProviderActionResult>;
  remove: (provider: ProProvider) => Promise<ProviderActionResult>;
};

const CURRENT_DISCLOSURE_VERSION = 1;

/**
 * The webview can select a provider, but it never receives or submits a key.
 * Native macOS UI owns entry, validation, and Keychain storage.
 */
export function tauriProProviderBridge(): ProProviderBridge {
  return {
    list: () => invoke<ProviderConfiguration[]>("provider_configurations"),
    configure: (provider) =>
      invoke<ProviderActionResult>("configure_provider_key", {
        provider,
        disclosureVersion: CURRENT_DISCLOSURE_VERSION,
      }),
    revalidate: (provider) =>
      invoke<ProviderActionResult>("revalidate_provider_key", { provider }),
    remove: (provider) =>
      invoke<ProviderActionResult>("remove_provider_key", { provider }),
  };
}
