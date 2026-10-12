// Diagnostics: forwards webview messages into Ferret's log file (see src-tauri/src/logging.rs),
// next to the backend's own lines. Logging must never break the app, so every call swallows
// its own failures.
import * as plugin from "@tauri-apps/plugin-log";

const send = (fn: (m: string) => Promise<void>, message: string) => {
  fn(message).catch(() => {});
};

export const log = {
  error: (message: string) => send(plugin.error, message),
  warn: (message: string) => send(plugin.warn, message),
  info: (message: string) => send(plugin.info, message),
  debug: (message: string) => send(plugin.debug, message),
};

/** Error text with its stack, when there is one. */
export function describeError(e: unknown): string {
  // WebView2's stacks start with "Name: message".
  if (e instanceof Error) return e.stack ?? `${e.name}: ${e.message}`;
  if (typeof e === "string") return e;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}

const MAX_ARGS = 500;

/** Command arguments for a log line: secrets redacted, long values cut short. */
function describeArgs(args: unknown): string {
  if (args === undefined) return "";
  let text: string;
  try {
    text = JSON.stringify(args, (k, v) => (/password/i.test(k) && v != null ? "<redacted>" : v)) ?? "";
  } catch {
    return " <unserializable args>";
  }
  return " " + (text.length > MAX_ARGS ? `${text.slice(0, MAX_ARGS)}… (${text.length} chars)` : text);
}

const REPEAT_MS = 60_000;
const lastSeen = new Map<string, number>();

/** A backend command that failed. User cancellations aren't failures, and the same
 *  failure repeating (a poll against a lost network share) is recorded once a minute. */
export function logCommandFailure(cmd: string, args: unknown, e: unknown) {
  const text = describeError(e);
  if (text === "Cancelled") return;
  const line = `command ${cmd} failed: ${text}${describeArgs(args)}`;
  const now = Date.now();
  if (now - (lastSeen.get(line) ?? -Infinity) < REPEAT_MS) return;
  if (lastSeen.size > 500) lastSeen.clear();
  lastSeen.set(line, now);
  log.warn(line);
}

/** Records uncaught errors and unhandled promise rejections. Call once at startup. */
export function installErrorHandlers() {
  window.addEventListener("error", (ev) => {
    const where = ev.filename ? ` (${ev.filename}:${ev.lineno}:${ev.colno})` : "";
    log.error(`uncaught: ${ev.error ? describeError(ev.error) : ev.message}${where}`);
  });
  window.addEventListener("unhandledrejection", (ev) => {
    log.error(`unhandled rejection: ${describeError(ev.reason)}`);
  });
}
