// Runs before the page first paints: the splash (index.html) takes the theme, accent and
// animation setting from the last run (saved by applyAppearance in src/lib/state.ts), so a
// light theme or another accent never flashes the defaults while Ferret starts.
(function () {
  try {
    var s = JSON.parse(localStorage.getItem("ferret:appearance") || "null") || {};
    var root = document.documentElement;
    var theme = s.theme || "dark";
    var dark = theme === "dark" || (theme === "auto" && window.matchMedia("(prefers-color-scheme: dark)").matches);
    root.dataset.theme = dark ? "dark" : "light";
    if (s.accent) root.style.setProperty("--accent", s.accent);
    if (s.motion === false) root.dataset.motion = "off";
  } catch (e) {
    // No saved appearance (first run, or storage unavailable): the defaults.
  }
})();

// The window starts hidden and appears with the app already drawn (showApp in src/App.tsx).
// A start slower than 400 ms (SPLASH_AFTER_MS there), say a busy disk, a folder on a network
// share or the dev server, shows it with this splash instead of nothing, and the splash fades
// out once the app is in.
setTimeout(function () {
  var splash = document.getElementById("splash");
  var ipc = window.__TAURI_INTERNALS__;
  if (!splash || !ipc) return;
  splash.dataset.shown = "1";
  ipc.invoke("show_window").catch(function () {});
}, 400);
