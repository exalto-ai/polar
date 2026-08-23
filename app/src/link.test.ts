import { describe, expect, it } from "vitest";
import { normalize } from "./link";

describe("link normalisation", () => {
  it("adds a scheme to what people actually paste", () => {
    // Without one the browser resolves it relative to the page, which in a
    // Tauri window means tauri://localhost/example.com.
    expect(normalize("example.com")).toBe("https://example.com");
    expect(normalize("example.com/a/b?c=1")).toBe("https://example.com/a/b?c=1");
  });

  it("leaves an explicit scheme alone", () => {
    expect(normalize("https://example.com")).toBe("https://example.com");
    expect(normalize("http://example.com")).toBe("http://example.com");
    expect(normalize("mailto:someone@example.com")).toBe("mailto:someone@example.com");
  });

  it("completes a protocol-relative link", () => {
    expect(normalize("//example.com")).toBe("https://example.com");
  });
});
