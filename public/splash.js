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
