import { beforeEach, describe, expect, it, vi } from "vitest";
import { installProProvider } from "./pro-provider";
import type { ProProviderBridge } from "./pro-provider-bridge";

beforeEach(() => {
  document.body.innerHTML = `
    <section id="provider-settings">
      <p id="provider-error" hidden></p>
      <div data-provider="openai">
        <span data-provider-status></span>
        <button data-provider-configure></button>
        <button data-provider-remove></button>
      </div>
      <div data-provider="anthropic">
        <span data-provider-status></span>
        <button data-provider-configure></button>
        <button data-provider-remove></button>
      </div>
    </section>`;
});

describe("provider settings", () => {
  it("shows Keychain presence without handling a secret", async () => {
    const bridge: ProProviderBridge = {
      list: vi.fn().mockResolvedValue([
        { provider: "openai", configured: true },
        { provider: "anthropic", configured: false },
      ]),
      configure: vi.fn(),
      remove: vi.fn(),
    };
    const controller = installProProvider(document, { bridge });
    controller.setActive(true);
    await vi.waitFor(() => {
      expect(document.querySelector('[data-provider="openai"]')?.textContent)
        .toContain("Key saved");
    });
    expect(document.body.textContent).not.toContain("Bearer");
    controller.destroy();
  });

  it("delegates key entry and removal to native commands", async () => {
    const bridge: ProProviderBridge = {
      list: vi.fn().mockResolvedValue([
        { provider: "openai", configured: false },
        { provider: "anthropic", configured: false },
      ]),
      configure: vi.fn().mockResolvedValue({
        outcome: "saved",
        configuration: { provider: "openai", configured: true },
      }),
      remove: vi.fn().mockResolvedValue({
        outcome: "removed",
        configuration: { provider: "openai", configured: false },
      }),
    };
    const controller = installProProvider(document, { bridge });
    controller.setActive(true);
    await vi.waitFor(() => expect(bridge.list).toHaveBeenCalledOnce());

    document.querySelector<HTMLButtonElement>(
      '[data-provider="openai"] [data-provider-configure]',
    )!.click();
    await vi.waitFor(() => expect(bridge.configure).toHaveBeenCalledWith("openai"));

    document.querySelector<HTMLButtonElement>(
      '[data-provider="openai"] [data-provider-remove]',
    )!.click();
    await vi.waitFor(() => expect(bridge.remove).toHaveBeenCalledWith("openai"));
    controller.destroy();
  });
});
