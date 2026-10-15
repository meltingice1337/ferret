// Unit tests (`npm test`): the frontend's pure logic, in plain Node, no window needed.
// The end-to-end run against the real app is `npm run test:e2e` (scripts/e2e).
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["tests/unit/**/*.test.ts"],
    environment: "node",
  },
});
