import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProChatBridge, SendChatRequest } from "./pro-chat-bridge";
import { installProChat } from "./pro-chat";

function markup(): string {
  return `
    <section id="pro-chat">
      <button id="pro-chat-new"></button>
      <p id="pro-chat-document"></p>
      <select id="pro-chat-provider"><option value=""></option><option value="openai">OpenAI</option></select>
      <select id="pro-chat-model"></select>
      <p id="pro-chat-error" hidden></p>
      <button id="pro-chat-retry" hidden></button>
      <p id="pro-chat-empty"></p>
      <ol id="pro-chat-messages" hidden></ol>
      <form id="pro-chat-form">
        <input id="pro-chat-consent" type="checkbox" />
        <textarea id="pro-chat-input"></textarea>
        <button id="pro-chat-send" type="submit"></button>
      </form>
    </section>
  `;
}

beforeEach(() => {
  document.body.innerHTML = markup();
});

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe("built-in chat", () => {
  it("shares the current snapshot only after the user acknowledges the notice", async () => {
    let sent: SendChatRequest | null = null;
    const bridge: ProChatBridge = {
      models: vi.fn().mockResolvedValue({
        provider: "openai",
        models: [{ id: "gpt-test", display_name: "GPT Test" }],
      }),
      send: vi.fn().mockImplementation(async (request: SendChatRequest) => {
        sent = request;
        return {
          text: "A clearer ending",
          provider: "openai",
          requested_model: "gpt-test",
          reported_model: "gpt-test-2026",
          wording_revision: "revision-1",
          complete: true,
        };
      }),
    };
    const controller = installProChat(document, { bridge });
    controller.setActive(true);
    controller.setDocument({
      id: "private-document-id",
      title: "Draft",
      snapshot: () => ({ type: "doc", content: [] }),
    });

    const provider = document.querySelector<HTMLSelectElement>("#pro-chat-provider")!;
    provider.value = "openai";
    provider.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(bridge.models).toHaveBeenCalledWith("openai");
      expect(bridge.models).toHaveBeenCalledTimes(1);
      expect([
        document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value,
        document.querySelector("#pro-chat-error")?.textContent,
      ]).toEqual(["gpt-test", ""]);
    });

    const input = document.querySelector<HTMLTextAreaElement>("#pro-chat-input")!;
    const send = document.querySelector<HTMLButtonElement>("#pro-chat-send")!;
    input.value = "Improve the ending";
    input.dispatchEvent(new Event("input"));
    expect(send.disabled).toBe(true);
    const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
    consent.checked = true;
    consent.dispatchEvent(new Event("change"));
    expect(send.disabled).toBe(false);

    document.querySelector<HTMLFormElement>("#pro-chat-form")!
      .dispatchEvent(new Event("submit", { cancelable: true }));
    await vi.waitFor(() => expect(sent).not.toBeNull());
    expect(sent).toMatchObject({
      document_title: "Draft",
      document: { type: "doc", content: [] },
      message: "Improve the ending",
      disclosure_version: 1,
    });
    expect(sent).not.toHaveProperty("document_id");
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-messages")?.textContent)
        .toContain("A clearer ending");
    });
    controller.destroy();
  });

  it("drops in-memory chat when the document changes", async () => {
    const bridge: ProChatBridge = {
      models: vi.fn().mockResolvedValue({
        provider: "openai",
        models: [{ id: "gpt-test", display_name: "GPT Test" }],
      }),
      send: vi.fn().mockResolvedValue({
        text: "Reply",
        provider: "openai",
        requested_model: "gpt-test",
        reported_model: null,
        wording_revision: "revision-1",
        complete: true,
      }),
    };
    const controller = installProChat(document, { bridge });
    controller.setActive(true);
    controller.setDocument({ id: "one", title: "One", snapshot: () => ({}) });
    const provider = document.querySelector<HTMLSelectElement>("#pro-chat-provider")!;
    provider.value = "openai";
    provider.dispatchEvent(new Event("change"));
    await vi.waitFor(() => {
      expect(bridge.models).toHaveBeenCalledWith("openai");
      expect(bridge.models).toHaveBeenCalledTimes(1);
      expect([
        document.querySelector<HTMLSelectElement>("#pro-chat-model")!.value,
        document.querySelector("#pro-chat-error")?.textContent,
      ]).toEqual(["gpt-test", ""]);
    });
    const consent = document.querySelector<HTMLInputElement>("#pro-chat-consent")!;
    consent.checked = true;
    consent.dispatchEvent(new Event("change"));
    const input = document.querySelector<HTMLTextAreaElement>("#pro-chat-input")!;
    input.value = "Question";
    input.dispatchEvent(new Event("input"));
    document.querySelector<HTMLFormElement>("#pro-chat-form")!
      .dispatchEvent(new Event("submit", { cancelable: true }));
    await vi.waitFor(() => {
      expect(document.querySelector("#pro-chat-messages")?.textContent).toContain("Reply");
    });

    controller.setDocument({ id: "two", title: "Two", snapshot: () => ({}) });
    expect(document.querySelector("#pro-chat-messages")?.textContent).toBe("");
    expect(document.querySelector("#pro-chat-document")?.textContent).toContain("Two");
    controller.destroy();
  });
});
