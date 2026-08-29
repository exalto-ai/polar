import { afterEach, describe, expect, it, vi } from "vitest";
import { writeClipboardText } from "./clipboard";

afterEach(() => {
  document.body.replaceChildren();
  vi.restoreAllMocks();
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
  Object.defineProperty(document, "execCommand", { configurable: true, value: undefined });
});

describe("clipboard compatibility", () => {
  it("uses the asynchronous Clipboard API when it is available", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    const legacyCopy = vi.fn();
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: legacyCopy,
    });

    await writeClipboardText("modern copy");

    expect(writeText).toHaveBeenCalledWith("modern copy");
    expect(legacyCopy).not.toHaveBeenCalled();
  });

  it("falls back without leaving a control or stealing focus", async () => {
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    const legacyCopy = vi.fn().mockReturnValue(true);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: legacyCopy,
    });

    await writeClipboardText("older WKWebView copy");

    expect(legacyCopy).toHaveBeenCalledWith("copy");
    expect(document.querySelector("textarea")).toBeNull();
    expect(document.activeElement).toBe(button);
  });

  it("falls back when the exposed Clipboard API rejects the write", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("permission denied"));
    const legacyCopy = vi.fn().mockReturnValue(true);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: legacyCopy,
    });

    await writeClipboardText("restricted WKWebView copy");

    expect(writeText).toHaveBeenCalledWith("restricted WKWebView copy");
    expect(legacyCopy).toHaveBeenCalledWith("copy");
  });

  it("keeps the modern error when both clipboard paths fail", async () => {
    const modernError = new Error("permission denied");
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn().mockRejectedValue(modernError) },
    });
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: vi.fn().mockReturnValue(false),
    });

    await expect(writeClipboardText("cannot copy")).rejects.toBe(modernError);
  });

  it("cleans up and reports an unavailable fallback", async () => {
    const button = document.createElement("button");
    document.body.append(button);
    button.focus();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    Object.defineProperty(document, "execCommand", {
      configurable: true,
      value: vi.fn().mockReturnValue(false),
    });

    await expect(writeClipboardText("cannot copy")).rejects.toThrow("copy is unavailable");
    expect(document.querySelector("textarea")).toBeNull();
    expect(document.activeElement).toBe(button);
  });
});
