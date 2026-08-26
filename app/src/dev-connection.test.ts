import { describe, expect, it } from "vitest";
import { isLoopbackAddress } from "./dev-connection";

describe("development capability endpoint", () => {
  it("accepts loopback sockets and rejects LAN or missing peers", () => {
    for (const address of [
      "127.0.0.1",
      "127.12.34.56",
      "::1",
      "0:0:0:0:0:0:0:1",
      "::ffff:127.0.0.1",
      "::1%lo0",
    ]) {
      expect(isLoopbackAddress(address), address).toBe(true);
    }
    for (const address of [
      undefined,
      "0.0.0.0",
      "192.168.1.5",
      "::ffff:10.0.0.2",
      "::2",
    ]) {
      expect(isLoopbackAddress(address), String(address)).toBe(false);
    }
  });
});
