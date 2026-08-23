import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // The provider touches WebSocket, requestAnimationFrame and document
    // visibility, so it needs a DOM even though none of it renders.
    environment: "happy-dom",
    include: ["src/**/*.test.ts"],
  },
});
