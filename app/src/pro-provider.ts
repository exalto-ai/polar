import { openUrl } from "@tauri-apps/plugin-opener";
import type {
  ProProvider,
  ProProviderBridge,
  ProviderActionResult,
  ProviderConfiguration,
  ProviderValidationStatus,
} from "./pro-provider-bridge";

const PROVIDERS: readonly ProProvider[] = ["openai", "anthropic"];
const PROVIDER_NAME: Record<ProProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};

type ProProviderOptions = {
  bridge?: ProProviderBridge | null;
  openExternal?: (url: string) => Promise<void>;
  onNotice?: (message: string, kind?: "info" | "error") => void;
  onConfigurationsChange?: (
    configurations: readonly ProviderConfiguration[],
  ) => void;
};

export type ProProviderController = {
  setActive(active: boolean): void;
  setChatBusy(provider: ProProvider | null): void;
  refresh(): Promise<void>;
  destroy(): void;
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing Pro provider element: ${selector}`);
  return value;
}

function formatDate(value: number | null): string | null {
  if (value === null || !Number.isFinite(value) || value <= 0) return null;
  const milliseconds = value < 1_000_000_000_000 ? value * 1000 : value;
  const date = new Date(milliseconds);
  if (!Number.isFinite(date.valueOf())) return null;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
}

function statusCopy(status: ProviderValidationStatus): string {
  switch (status) {
    case "model_access_checked":
      return "The provider accepted this key and returned its model catalog.";
    case "model_unavailable":
      return "The provider accepted this key, but returned no available models.";
    case "invalid_key_format":
      return "That entry is not a usable API key. Check it and try again.";
    case "credential_or_access_invalid":
      return "The provider did not accept that key or its account access.";
    case "permission_denied":
      return "The key does not have permission to read the provider’s model catalog.";
    case "billing_unavailable":
      return "The provider reports a billing or credit problem for this account.";
    case "spend_or_usage_limit":
      return "This account has reached a provider spending, credit, or usage limit.";
    case "rate_limited":
      return "The provider is limiting requests right now. Try again shortly.";
    case "unsupported_region":
      return "The provider reports that this account or region is not supported.";
    case "provider_unavailable":
      return "The provider is temporarily unavailable. Your saved key was not changed.";
    case "timeout":
      return "The provider did not respond in time. Your saved key was not changed.";
    case "network_or_tls_failure":
      return "Proof of Thought could not securely reach the provider. Your saved key was not changed.";
    case "invalid_provider_response":
      return "The provider returned an unexpected response. Your saved key was not changed.";
    case "credential_missing":
      return "The saved key is no longer available in Keychain.";
    case "not_checked":
      return "This key has not been checked yet.";
  }
}

function shortError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  const clean = message.replace(/[\r\n\t]+/g, " ").trim();
  return clean ? clean.slice(0, 180) : "The provider setup could not be completed.";
}

function configurationSnapshot(
  configurations: Iterable<ProviderConfiguration>,
): string {
  return JSON.stringify(
    [...configurations].sort((left, right) =>
      left.provider.localeCompare(right.provider)
    ),
  );
}

async function defaultOpenExternal(url: string): Promise<void> {
  const tauri = Boolean(
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__,
  );
  if (tauri) {
    await openUrl(url);
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

export function installProProvider(
  root: Document,
  options: ProProviderOptions = {},
): ProProviderController {
  const panel = required<HTMLElement>(root, "#ai-pro-panel");
  const costAcknowledgement = required<HTMLInputElement>(root, "#pro-cost-acknowledgement");
  const error = required<HTMLElement>(root, "#pro-provider-error");
  const retry = required<HTMLButtonElement>(root, "#pro-provider-retry");
  const live = required<HTMLElement>(root, "#pro-provider-live");
  const bridge = options.bridge ?? null;
  const openExternal = options.openExternal ?? defaultOpenExternal;
  const configurations = new Map<ProProvider, ProviderConfiguration>();
  const attemptStatus = new Map<ProProvider, ProviderValidationStatus>();
  const disposers: Array<() => void> = [];
  let active = false;
  let busy: ProProvider | null = null;
  let chatBusy: ProProvider | null = null;
  let loadError: string | null = null;
  let removeConfirmation: ProProvider | null = null;
  let removeReturnFocus: HTMLButtonElement | null = null;
  let destroyed = false;
  let refreshGeneration = 0;

  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement,
    event: K,
    listener: (event: HTMLElementEventMap[K]) => void,
  ): void {
    target.addEventListener(event, listener);
    disposers.push(() => target.removeEventListener(event, listener));
  }

  function card(provider: ProProvider): HTMLElement {
    return required(panel, `[data-pro-provider-card="${provider}"]`);
  }

  function renderProvider(provider: ProProvider): void {
    const container = card(provider);
    const configuration = configurations.get(provider);
    const configured = configuration?.configured === true;
    const removalPending = configuration?.removal_pending === true;
    const status = attemptStatus.get(provider) ?? configuration?.validation_status ?? "not_checked";
    const statusElement = required<HTMLElement>(container, "[data-pro-provider-status]");
    const detail = required<HTMLElement>(container, "[data-pro-provider-detail]");
    const configure = required<HTMLButtonElement>(
      container,
      '[data-pro-provider-action="configure"]',
    );
    const revalidate = required<HTMLButtonElement>(
      container,
      '[data-pro-provider-action="revalidate"]',
    );
    const remove = required<HTMLButtonElement>(
      container,
      '[data-pro-provider-action="remove"]',
    );
    const confirmation = required<HTMLElement>(
      container,
      "[data-pro-provider-remove-confirmation]",
    );

    statusElement.dataset.status = removalPending
      ? "attention"
      : configured
      ? status === "model_access_checked" || status === "model_unavailable"
        ? "checked"
        : "attention"
      : "unset";
    statusElement.textContent = removalPending
      ? "Removal unfinished"
      : configured
      ? status === "model_access_checked"
        ? "Model catalog checked"
        : status === "model_unavailable"
          ? "Key saved"
          : "Needs attention"
      : "Not set up";

    if (removalPending) {
      detail.textContent = "The local Keychain removal did not finish. Try again before using this provider.";
    } else if (attemptStatus.has(provider)) {
      detail.textContent = statusCopy(status);
    } else if (configured && configuration) {
      const checked = formatDate(configuration.last_checked_at);
      const models = configuration.model_count;
      const pieces = [
        statusCopy(status),
        checked ? `Last checked ${checked}.` : null,
        models !== null ? `${models} model${models === 1 ? "" : "s"} visible.` : null,
      ].filter((value): value is string => value !== null);
      detail.textContent = pieces.join(" ");
    } else {
      detail.textContent = "Add a key in a native secure field, then Proof of Thought will check model-list access.";
    }

    configure.textContent = configured ? "Replace key" : "Add key";
    configure.hidden = removalPending;
    const operationBlocked = busy !== null || chatBusy !== null;
    configure.disabled = operationBlocked || bridge === null || !costAcknowledgement.checked;
    revalidate.hidden = !configured;
    revalidate.disabled = operationBlocked || bridge === null;
    remove.hidden = !configured && !removalPending;
    remove.textContent = removalPending ? "Finish removal" : "Remove";
    remove.disabled = operationBlocked || bridge === null;
    confirmation.hidden = removeConfirmation !== provider;
    container.setAttribute(
      "aria-busy",
      String(busy === provider || chatBusy === provider),
    );
  }

  function render(): void {
    panel.setAttribute("aria-busy", String(busy !== null || chatBusy !== null));
    error.hidden = loadError === null;
    error.firstElementChild!.textContent = loadError ?? "";
    for (const provider of PROVIDERS) renderProvider(provider);
  }

  function closeRemoveConfirmation(): void {
    removeConfirmation = null;
    render();
    const returnFocus = removeReturnFocus;
    removeReturnFocus = null;
    returnFocus?.focus();
  }

  function saveResult(result: ProviderActionResult): void {
    configurations.set(result.configuration.provider, result.configuration);
    if (result.attempt_status === null || result.outcome === "saved") {
      attemptStatus.delete(result.configuration.provider);
    } else {
      attemptStatus.set(result.configuration.provider, result.attempt_status);
    }
    options.onConfigurationsChange?.([...configurations.values()]);
  }

  async function refresh(): Promise<void> {
    if (bridge === null || destroyed) {
      loadError = bridge === null ? "Secure provider setup is unavailable in this build." : null;
      render();
      return;
    }
    const generation = ++refreshGeneration;
    loadError = null;
    render();
    try {
      const values = await bridge.list();
      if (destroyed || generation !== refreshGeneration) return;
      const previous = configurationSnapshot(configurations.values());
      configurations.clear();
      for (const configuration of values) {
        if (PROVIDERS.includes(configuration.provider)) {
          configurations.set(configuration.provider, configuration);
        }
      }
      if (configurationSnapshot(configurations.values()) !== previous) {
        options.onConfigurationsChange?.([...configurations.values()]);
      }
    } catch (cause) {
      if (destroyed || generation !== refreshGeneration) return;
      loadError = shortError(cause);
    }
    render();
  }

  async function configure(provider: ProProvider): Promise<void> {
    if (bridge === null || busy !== null || !costAcknowledgement.checked) return;
    busy = provider;
    loadError = null;
    attemptStatus.delete(provider);
    render();
    try {
      const result = await bridge.configure(provider);
      if (destroyed) return;
      saveResult(result);
      if (result.outcome === "cancelled") {
        live.textContent = "No changes were made.";
      } else if (result.outcome === "validation_failed") {
        live.textContent = `${PROVIDER_NAME[provider]} was not added. ${statusCopy(result.attempt_status ?? "invalid_provider_response")}`;
      } else {
        live.textContent = `${PROVIDER_NAME[provider]} key saved in your Mac login Keychain. ${statusCopy(result.attempt_status ?? result.configuration.validation_status)}`;
      }
    } catch (cause) {
      if (destroyed) return;
      loadError = shortError(cause);
      options.onNotice?.(`Could not set up ${PROVIDER_NAME[provider]}: ${loadError}`, "error");
    } finally {
      if (!destroyed) {
        busy = null;
        render();
        card(provider)
          .querySelector<HTMLButtonElement>('[data-pro-provider-action="configure"]')
          ?.focus();
      }
    }
  }

  async function revalidate(provider: ProProvider): Promise<void> {
    if (bridge === null || busy !== null) return;
    busy = provider;
    loadError = null;
    attemptStatus.delete(provider);
    render();
    try {
      const result = await bridge.revalidate(provider);
      if (destroyed) return;
      saveResult(result);
      live.textContent = `${PROVIDER_NAME[provider]} check finished. ${statusCopy(result.attempt_status ?? result.configuration.validation_status)}`;
    } catch (cause) {
      if (destroyed) return;
      loadError = shortError(cause);
      options.onNotice?.(`Could not recheck ${PROVIDER_NAME[provider]}: ${loadError}`, "error");
    } finally {
      if (!destroyed) {
        busy = null;
        render();
        card(provider)
          .querySelector<HTMLButtonElement>('[data-pro-provider-action="revalidate"]')
          ?.focus();
      }
    }
  }

  async function remove(provider: ProProvider): Promise<void> {
    if (bridge === null || busy !== null) return;
    busy = provider;
    loadError = null;
    render();
    try {
      const result = await bridge.remove(provider);
      if (destroyed) return;
      saveResult(result);
      removeConfirmation = null;
      removeReturnFocus = null;
      live.textContent = `${PROVIDER_NAME[provider]} was removed from Proof of Thought. The provider-side key was not revoked.`;
    } catch (cause) {
      if (destroyed) return;
      loadError = shortError(cause);
      options.onNotice?.(`Could not remove ${PROVIDER_NAME[provider]}: ${loadError}`, "error");
    } finally {
      if (!destroyed) {
        busy = null;
        render();
        card(provider)
          .querySelector<HTMLButtonElement>('[data-pro-provider-action="configure"]')
          ?.focus();
      }
    }
  }

  listen(costAcknowledgement, "change", render);
  listen(retry, "click", () => void refresh());
  for (const provider of PROVIDERS) {
    const container = card(provider);
    listen(
      required(container, '[data-pro-provider-action="configure"]'),
      "click",
      () => void configure(provider),
    );
    listen(
      required(container, '[data-pro-provider-action="revalidate"]'),
      "click",
      () => void revalidate(provider),
    );
    listen(required(container, '[data-pro-provider-action="remove"]'), "click", (event) => {
      removeConfirmation = provider;
      removeReturnFocus = event.currentTarget as HTMLButtonElement;
      render();
      queueMicrotask(() =>
        card(provider)
          .querySelector<HTMLButtonElement>('[data-pro-provider-action="cancel-remove"]')
          ?.focus()
      );
    });
    listen(required(container, '[data-pro-provider-action="cancel-remove"]'), "click", () => {
      closeRemoveConfirmation();
    });
    listen(
      required(container, '[data-pro-provider-action="confirm-remove"]'),
      "click",
      () => void remove(provider),
    );
    listen(required(container, "[data-pro-provider-key-link]"), "click", (event) => {
      event.preventDefault();
      const anchor = event.currentTarget as HTMLAnchorElement;
      void openExternal(anchor.href).catch((cause) => {
        loadError = shortError(cause);
        render();
      });
    });
  }

  const onKeydown = (event: KeyboardEvent) => {
    if (event.key !== "Escape" || removeConfirmation === null || busy !== null) return;
    event.preventDefault();
    closeRemoveConfirmation();
  };
  root.addEventListener("keydown", onKeydown);
  disposers.push(() => root.removeEventListener("keydown", onKeydown));

  render();

  return {
    setActive(nextActive) {
      const becameActive = nextActive && !active;
      active = nextActive;
      if (becameActive && busy === null) void refresh();
    },
    setChatBusy(provider) {
      chatBusy = provider;
      render();
    },
    refresh,
    destroy() {
      destroyed = true;
      active = false;
      refreshGeneration += 1;
      for (const dispose of disposers.splice(0)) dispose();
    },
  };
}
