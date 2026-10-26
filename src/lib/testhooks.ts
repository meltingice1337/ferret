// Handles for the end-to-end tests (scripts/e2e) and for debugging a dev build: the app's own
// state, operations and backend calls, so a test can set things up and check results. Only in
// dev builds and the e2e test build (`vite build --mode e2e`); never in a release.
import * as api from "./api";
import * as sizes from "./foldersizes";
import * as ops from "./ops";
import * as state from "./state";

if (import.meta.env.DEV || import.meta.env.MODE === "e2e") {
  // Things the page's security policy (CSP) refused: they fail silently (no error is thrown),
  // like code highlighting's colors once an inline <style> crept into index.html.
  const csp: string[] = [];
  document.addEventListener("securitypolicyviolation", (e) => csp.push(`${e.violatedDirective}: ${e.blockedURI || "inline"}`));
  (window as any).__ferretTest = { api: api.api, ops, state, sizes, csp };
}
