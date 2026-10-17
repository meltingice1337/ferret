// Unit tests (`yarn test`): the frontend's pure logic, in plain Node, no window needed.
// The end-to-end run against the real app is `yarn test:e2e` (scripts/e2e).
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["tests/unit/**/*.test.ts"],
    environment: "node",
  },
});
