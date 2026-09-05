import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  AI_SUPPORT_PATH_STORAGE_KEY,
  installAiSupport,
  readAiSupportPath,
  writeAiSupportPath,
} from "./ai-support";

function memoryStorage(initial: Record<string, string> = {}): Storage {
  const values = new Map(Object.entries(initial));
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => values.delete(key),
    setItem: (key, value) => values.set(key, value),
  };
}

beforeEach(() => {
  document.body.innerHTML = `
    <header><button id="ai-support-toggle" aria-expanded="false"></button></header>
    <div id="editor"><div class="tiptap" tabindex="0"></div></div>
    <aside id="ai-support-sidebar" hidden>
      <h2 id="ai-support-title"></h2>
      <button id="ai-sidebar-close"></button>
      <p id="ai-mode-description"></p>
      <button data-ai-mode="connected"></button>
      <button data-ai-mode="builtin"></button>
      <button data-ai-mode="basic"></button>
      <section id="ai-connect-panel" hidden>
        <button id="reviewer-add"></button>
        <button id="reviewer-refresh"></button>
        <p id="reviewer-error" hidden></p>
        <p id="reviewer-empty"></p>
        <ul id="reviewer-list"></ul>
        <form id="reviewer-form" hidden>
          <h3 id="reviewer-form-title"></h3>
          <select id="reviewer-client"><option value="codex">Codex</option></select>
          <input id="reviewer-label" />
          <select id="reviewer-scope"><option value="current">Current</option><option value="all">All</option></select>
          <p id="reviewer-current"></p>
          <button id="reviewer-cancel" type="button"></button>
        </form>
        <section id="reviewer-setup" hidden>
          <p id="reviewer-setup-text"></p>
          <p id="reviewer-setup-name"></p>
          <pre id="reviewer-setup-command"></pre>
          <button id="reviewer-copy"></button>
          <button id="reviewer-setup-done"></button>
        </section>
      </section>
      <section id="ai-pro-panel" hidden></section>
      <section id="ai-basic-panel" hidden></section>
    </aside>
    <div id="ai-onboarding" hidden>
      <section role="dialog">
        <button id="ai-onboarding-close"></button>
        <h1 id="ai-onboarding-title" tabindex="-1"></h1>
        <button data-ai-mode="connected">Connect</button>
        <button data-ai-mode="builtin">Built in</button>
        <button data-ai-mode="basic">Basic</button>
      </section>
    </div>
  `;
});

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("AI support paths", () => {
  it("waits for successful document startup before showing first-launch choice", async () => {
    const storage = memoryStorage();
    const controller = installAiSupport(document, { storage });
    const onboarding = document.querySelector<HTMLElement>("#ai-onboarding")!;

    expect(onboarding.hidden).toBe(true);
    document.querySelector<HTMLButtonElement>("#ai-support-toggle")!.click();
    expect(onboarding.hidden).toBe(true);
    expect(controller.showOnboardingIfNeeded()).toBe(true);
    await Promise.resolve();

    expect(onboarding.hidden).toBe(false);
    expect(controller.isOpen()).toBe(true);
    expect(document.activeElement).toBe(
      document.querySelector("#ai-onboarding-title"),
    );
    expect(document.querySelector("header")!.hasAttribute("inert")).toBe(true);
    controller.destroy();
  });

  it("persists the connected path and opens its setup without changing permissions", async () => {
    const storage = memoryStorage();
    const controller = installAiSupport(document, { storage });
    controller.showOnboardingIfNeeded();

    document
      .querySelector<HTMLButtonElement>(
        "#ai-onboarding [data-ai-mode='connected']",
      )!
      .click();
    await Promise.resolve();

    expect(controller.path()).toBe("connected");
    expect(controller.isOpen()).toBe(true);
    expect(document.querySelector<HTMLElement>("#ai-connect-panel")!.hidden).toBe(
      false,
    );
    expect(readAiSupportPath(storage)).toBe("connected");
    expect(document.querySelector("[inert]")).toBeNull();
    controller.destroy();
  });

  it("returns to the editor when basic is chosen", async () => {
    const controller = installAiSupport(document, { storage: memoryStorage() });
    controller.showOnboardingIfNeeded();
    document
      .querySelector<HTMLButtonElement>("#ai-onboarding [data-ai-mode='basic']")!
      .click();
    await Promise.resolve();

    expect(controller.path()).toBe("basic");
    expect(controller.isOpen()).toBe(false);
    expect(document.activeElement).toBe(document.querySelector(".tiptap"));
    controller.destroy();
  });

  it("skips onboarding when a preference already exists", () => {
    const storage = memoryStorage({
      [AI_SUPPORT_PATH_STORAGE_KEY]: JSON.stringify({
        version: 1,
        path: "builtin",
      }),
    });
    const controller = installAiSupport(document, { storage });

    expect(controller.path()).toBe("builtin");
    expect(controller.showOnboardingIfNeeded()).toBe(false);
    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(
      true,
    );
    controller.destroy();
  });

  it("treats corrupt or unavailable storage as an unset preference", () => {
    const corrupt = memoryStorage({ [AI_SUPPORT_PATH_STORAGE_KEY]: "not json" });
    expect(readAiSupportPath(corrupt)).toBeNull();
    expect(writeAiSupportPath(null, "connected")).toBe(false);

    const unavailable: Storage = {
      ...memoryStorage(),
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(writeAiSupportPath(unavailable, "basic")).toBe(false);
  });

  it("keeps the existing non-blocking sidebar close behavior", async () => {
    const storage = memoryStorage();
    writeAiSupportPath(storage, "connected");
    const controller = installAiSupport(document, { storage });
    const toggle = document.querySelector<HTMLButtonElement>("#ai-support-toggle")!;
    toggle.click();
    await Promise.resolve();

    expect(controller.isOpen()).toBe(true);
    expect(document.activeElement).toBe(
      document.querySelector("#ai-sidebar-close"),
    );
    document.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
    );
    expect(controller.isOpen()).toBe(false);
    expect(document.activeElement).toBe(toggle);
    controller.destroy();
  });

  it("keeps focus on the mode control when switching inside the sidebar", async () => {
    const storage = memoryStorage();
    writeAiSupportPath(storage, "connected");
    const controller = installAiSupport(document, { storage });
    document.querySelector<HTMLButtonElement>("#ai-support-toggle")!.click();
    await Promise.resolve();
    const basic = document.querySelector<HTMLButtonElement>(
      "#ai-support-sidebar [data-ai-mode='basic']",
    )!;
    basic.focus();
    basic.click();
    await Promise.resolve();

    expect(controller.path()).toBe("basic");
    expect(controller.isOpen()).toBe(true);
    expect(document.activeElement).toBe(basic);
    controller.destroy();
  });

  it("restores focus when another window supplies the saved path", async () => {
    const storage = memoryStorage();
    const controller = installAiSupport(document, { storage });
    controller.showOnboardingIfNeeded();
    await Promise.resolve();
    expect(document.activeElement).toBe(
      document.querySelector("#ai-onboarding-title"),
    );

    writeAiSupportPath(storage, "connected");
    window.dispatchEvent(
      new StorageEvent("storage", { key: AI_SUPPORT_PATH_STORAGE_KEY }),
    );
    await Promise.resolve();

    expect(controller.path()).toBe("connected");
    expect(controller.isOpen()).toBe(false);
    expect(document.querySelector<HTMLElement>("#ai-onboarding")!.hidden).toBe(
      true,
    );
    expect(document.activeElement).toBe(document.querySelector(".tiptap"));
    controller.destroy();
  });
});
