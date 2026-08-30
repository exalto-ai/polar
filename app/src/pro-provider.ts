import type {
  ProProvider,
  ProProviderBridge,
  ProviderConfiguration,
} from "./pro-provider-bridge";

const PROVIDERS: readonly ProProvider[] = ["openai", "anthropic"];
const NAMES: Record<ProProvider, string> = {
  openai: "OpenAI",
  anthropic: "Anthropic",
};

type Options = {
  bridge?: ProProviderBridge | null;
  onNotice?: (message: string, kind?: "info" | "error") => void;
};

export type ProProviderController = {
  setActive(active: boolean): void;
  destroy(): void;
};

function required<T extends Element>(root: ParentNode, selector: string): T {
  const value = root.querySelector<T>(selector);
  if (!value) throw new Error(`missing provider element: ${selector}`);
  return value;
}

function oneLine(error: unknown): string {
  const value = error instanceof Error ? error.message : String(error);
  return value.replace(/[\r\n\t]+/g, " ").trim().slice(0, 180) ||
    "Provider setup failed.";
}

export function installProProvider(
  root: Document,
  options: Options = {},
): ProProviderController {
  const panel = required<HTMLElement>(root, "#provider-settings");
  const error = required<HTMLElement>(panel, "#provider-error");
  const bridge = options.bridge ?? null;
  const configured = new Map<ProProvider, boolean>();
  const disposers: Array<() => void> = [];
  let active = false;
  let busy: ProProvider | null = null;
  let destroyed = false;
  let generation = 0;

  function card(provider: ProProvider): HTMLElement {
    return required(panel, `[data-provider="${provider}"]`);
  }

  function render(): void {
    panel.setAttribute("aria-busy", String(busy !== null));
    for (const provider of PROVIDERS) {
      const container = card(provider);
      const isConfigured = configured.get(provider) === true;
      const status = required<HTMLElement>(container, "[data-provider-status]");
      const configure = required<HTMLButtonElement>(container, "[data-provider-configure]");
      const remove = required<HTMLButtonElement>(container, "[data-provider-remove]");
      status.textContent = isConfigured ? "Key saved" : "No key";
      status.dataset.configured = String(isConfigured);
      configure.textContent = isConfigured ? "Replace" : "Add key";
      configure.disabled = busy !== null || bridge === null;
      remove.hidden = !isConfigured;
      remove.disabled = busy !== null || bridge === null;
    }
  }

  function apply(configuration: ProviderConfiguration): void {
    if (PROVIDERS.includes(configuration.provider)) {
      configured.set(configuration.provider, configuration.configured);
    }
  }

  async function refresh(): Promise<void> {
    if (!active || bridge === null || destroyed) return;
    const request = ++generation;
    error.hidden = true;
    try {
      const values = await bridge.list();
      if (destroyed || request !== generation) return;
      configured.clear();
      values.forEach(apply);
    } catch (cause) {
      if (destroyed || request !== generation) return;
      error.textContent = oneLine(cause);
      error.hidden = false;
    }
    render();
  }

  async function configure(provider: ProProvider): Promise<void> {
    if (bridge === null || busy !== null) return;
    busy = provider;
    error.hidden = true;
    render();
    try {
      const result = await bridge.configure(provider);
      if (destroyed) return;
      apply(result.configuration);
      if (result.outcome === "saved") {
        options.onNotice?.(`${NAMES[provider]} key saved in Keychain.`);
      }
    } catch (cause) {
      if (destroyed) return;
      error.textContent = oneLine(cause);
      error.hidden = false;
      options.onNotice?.(`Could not save ${NAMES[provider]} key.`, "error");
    } finally {
      if (!destroyed) {
        busy = null;
        render();
      }
    }
  }

  async function remove(provider: ProProvider): Promise<void> {
    if (bridge === null || busy !== null) return;
    busy = provider;
    error.hidden = true;
    render();
    try {
      const result = await bridge.remove(provider);
      if (destroyed) return;
      apply(result.configuration);
      if (result.outcome === "removed") {
        options.onNotice?.(`${NAMES[provider]} key removed from Keychain.`);
      }
    } catch (cause) {
      if (destroyed) return;
      error.textContent = oneLine(cause);
      error.hidden = false;
      options.onNotice?.(`Could not remove ${NAMES[provider]} key.`, "error");
    } finally {
      if (!destroyed) {
        busy = null;
        render();
      }
    }
  }

  for (const provider of PROVIDERS) {
    const configureButton = required<HTMLButtonElement>(card(provider), "[data-provider-configure]");
    const removeButton = required<HTMLButtonElement>(card(provider), "[data-provider-remove]");
    const onConfigure = () => void configure(provider);
    const onRemove = () => void remove(provider);
    configureButton.addEventListener("click", onConfigure);
    removeButton.addEventListener("click", onRemove);
    disposers.push(
      () => configureButton.removeEventListener("click", onConfigure),
      () => removeButton.removeEventListener("click", onRemove),
    );
  }

  render();
  return {
    setActive(next) {
      const opened = next && !active;
      active = next;
      if (opened) void refresh();
    },
    destroy() {
      destroyed = true;
      generation += 1;
      disposers.splice(0).forEach((dispose) => dispose());
    },
  };
}
