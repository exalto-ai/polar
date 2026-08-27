import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import markup from "../index.html?raw";
import { installProProvider } from "./pro-provider";
import type {
  ProProvider,
  ProProviderBridge,
  ProviderActionResult,
  ProviderConfiguration,
  ProviderValidationStatus,
} from "./pro-provider-bridge";

function fixture(): void {
  document.body.innerHTML = markup.match(/<body>([\s\S]*?)<\/body>/)?.[1] ?? "";
}

function configuration(
  provider: ProProvider,
  configured = false,
  validationStatus: ProviderValidationStatus = "not_checked",
): ProviderConfiguration {
  return {
    provider,
    configured,
    removal_pending: false,
    validation_status: validationStatus,
    last_checked_at: configured ? 1_777_000_000 : null,
    last_validated_at: configured ? 1_777_000_000 : null,
    model_count: configured ? 4 : null,
    request_id: null,
    disclosure_version: configured ? 1 : null,
    charges_acknowledged_at: configured ? 1_777_000_000 : null,
  };
}

function result(
  outcome: ProviderActionResult["outcome"],
  value: ProviderConfiguration,
  attemptStatus: ProviderValidationStatus | null,
): ProviderActionResult {
  return {
    outcome,
    configuration: value,
    attempt_status: attemptStatus,
    request_id: null,
  };
}

function providerButton(provider: ProProvider, action: string): HTMLButtonElement {
  return document.querySelector<HTMLButtonElement>(
    `[data-pro-provider-card="${provider}"] [data-pro-provider-action="${action}"]`,
  )!;
}

beforeEach(fixture);
afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("Pro provider setup", () => {
  it("requires cost acknowledgement before native Add or Replace", async () => {
    const saved = configuration("openai", true, "model_access_checked");
    const configure = vi.fn().mockResolvedValue(
      result("saved", saved, "model_access_checked"),
    );
    const bridge: ProProviderBridge = {
      list: vi.fn().mockResolvedValue([
        configuration("openai"),
        configuration("anthropic"),
      ]),
      configure,
      revalidate: vi.fn(),
      remove: vi.fn(),
    };
    const controller = installProProvider(document, { bridge });
    controller.setActive(true);
    await vi.waitFor(() => expect(bridge.list).toHaveBeenCalledTimes(1));

    const add = providerButton("openai", "configure");
    expect(add.disabled).toBe(true);
    add.click();
    expect(configure).not.toHaveBeenCalled();

    const acknowledgement = document.querySelector<HTMLInputElement>(
      "#pro-cost-acknowledgement",
    )!;
    acknowledgement.checked = true;
    acknowledgement.dispatchEvent(new Event("change", { bubbles: true }));
    expect(add.disabled).toBe(false);
    add.click();

    await vi.waitFor(() => expect(configure).toHaveBeenCalledWith("openai"));
    await vi.waitFor(() => expect(add.textContent).toBe("Replace key"));
    expect(
      document.querySelector(
        '[data-pro-provider-card="openai"] [data-pro-provider-status]',
      )?.textContent,
    ).toBe("Model catalog checked");
    expect(document.querySelector("#pro-provider-live")?.textContent).toContain(
      "Mac login Keychain",
    );
    expect(document.querySelector("#pro-provider-live")?.textContent).not.toContain(
      "ready",
    );
    controller.destroy();
  });

  it("keeps an existing configuration after a failed replacement", async () => {
    const existing = configuration("anthropic", true, "model_access_checked");
    const bridge: ProProviderBridge = {
      list: vi.fn().mockResolvedValue([existing]),
      configure: vi.fn().mockResolvedValue(
        result("validation_failed", existing, "network_or_tls_failure"),
      ),
      revalidate: vi.fn(),
      remove: vi.fn(),
    };
    const controller = installProProvider(document, { bridge });
    controller.setActive(true);
    await vi.waitFor(() => expect(bridge.list).toHaveBeenCalled());
    const acknowledgement = document.querySelector<HTMLInputElement>(
      "#pro-cost-acknowledgement",
    )!;
    acknowledgement.checked = true;
    acknowledgement.dispatchEvent(new Event("change", { bubbles: true }));

    providerButton("anthropic", "configure").click();

    await vi.waitFor(() =>
      expect(
        document.querySelector(
          '[data-pro-provider-card="anthropic"] [data-pro-provider-detail]',
        )?.textContent,
      ).toContain("saved key was not changed"),
    );
    expect(providerButton("anthropic", "configure").textContent).toBe("Replace key");
    expect(providerButton("anthropic", "remove").hidden).toBe(false);
    controller.destroy();
  });

  it("rechecks a saved key and distinguishes local removal from provider revocation", async () => {
    const existing = configuration("openai", true, "model_access_checked");
    const checked = { ...existing, validation_status: "rate_limited" as const };
    const removed = configuration("openai");
    const bridge: ProProviderBridge = {
      list: vi.fn().mockResolvedValue([existing]),
      configure: vi.fn(),
      revalidate: vi.fn().mockResolvedValue(result("checked", checked, "rate_limited")),
      remove: vi.fn().mockResolvedValue(result("removed", removed, null)),
    };
    const controller = installProProvider(document, { bridge });
    controller.setActive(true);
    await vi.waitFor(() => expect(bridge.list).toHaveBeenCalled());

    providerButton("openai", "revalidate").click();
    await vi.waitFor(() => expect(bridge.revalidate).toHaveBeenCalledWith("openai"));
    expect(
      document.querySelector(
        '[data-pro-provider-card="openai"] [data-pro-provider-detail]',
      )?.textContent,
    ).toContain("limiting requests");

    providerButton("openai", "remove").click();
    const confirmation = document.querySelector<HTMLElement>(
      '[data-pro-provider-card="openai"] [data-pro-provider-remove-confirmation]',
    )!;
    expect(confirmation.hidden).toBe(false);
    expect(confirmation.textContent).toContain("does not revoke the key");
    providerButton("openai", "confirm-remove").click();

    await vi.waitFor(() => expect(bridge.remove).toHaveBeenCalledWith("openai"));
    await vi.waitFor(() => expect(providerButton("openai", "remove").hidden).toBe(true));
    expect(document.querySelector("#pro-provider-live")?.textContent).toContain(
      "provider-side key was not revoked",
    );
    controller.destroy();
  });

  it("opens only the provider’s official key page", async () => {
    const openExternal = vi.fn().mockResolvedValue(undefined);
    const controller = installProProvider(document, {
      bridge: {
        list: vi.fn().mockResolvedValue([]),
        configure: vi.fn(),
        revalidate: vi.fn(),
        remove: vi.fn(),
      },
      openExternal,
    });
    const link = document.querySelector<HTMLAnchorElement>(
      '[data-pro-provider-card="anthropic"] [data-pro-provider-key-link]',
    )!;

    link.click();

    expect(openExternal).toHaveBeenCalledWith("https://platform.claude.com/settings/keys");
    controller.destroy();
  });
});
