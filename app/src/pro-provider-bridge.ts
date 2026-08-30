import { invoke } from "@tauri-apps/api/core";

export type ProProvider = "openai" | "anthropic";

export type ProviderConfiguration = {
  provider: ProProvider;
  configured: boolean;
};

export type ProviderActionResult = {
  outcome: "saved" | "removed" | "cancelled";
  configuration: ProviderConfiguration;
};

export type ProProviderBridge = {
  list(): Promise<ProviderConfiguration[]>;
  configure(provider: ProProvider): Promise<ProviderActionResult>;
  remove(provider: ProProvider): Promise<ProviderActionResult>;
};

/** Provider identifiers cross IPC. API keys do not. */
export function tauriProProviderBridge(): ProProviderBridge {
  return {
    list: () => invoke<ProviderConfiguration[]>("provider_configurations"),
    configure: (provider) =>
      invoke<ProviderActionResult>("configure_provider_key", { provider }),
    remove: (provider) =>
      invoke<ProviderActionResult>("remove_provider_key", { provider }),
  };
}
