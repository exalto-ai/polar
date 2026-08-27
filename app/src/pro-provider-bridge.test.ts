import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { tauriProProviderBridge } from "./pro-provider-bridge";

describe("Pro provider bridge", () => {
  beforeEach(() => invoke.mockReset().mockResolvedValue([]));

  it("passes only a fixed provider and disclosure version into native code", async () => {
    const bridge = tauriProProviderBridge();

    await bridge.list();
    await bridge.configure("openai");
    await bridge.revalidate("anthropic");
    await bridge.remove("openai");

    expect(invoke.mock.calls).toEqual([
      ["provider_configurations"],
      ["configure_provider_key", { provider: "openai", disclosureVersion: 1 }],
      ["revalidate_provider_key", { provider: "anthropic" }],
      ["remove_provider_key", { provider: "openai" }],
    ]);
    expect(JSON.stringify(invoke.mock.calls)).not.toMatch(/api.?key|secret|password/i);
  });
});
