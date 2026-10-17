// Scale and theme before first paint; src/display.ts owns them afterwards.
// A file rather than an inline script, so the CSP allows scripts from 'self' only.
(function () {
  var display = {};
  try {
    display = JSON.parse(localStorage.getItem("vigie.display")) || {};
  } catch (e) {}
  var scales = [87.5, 100, 112.5, 125];
  var root = document.documentElement;
  if (scales.indexOf(display.scale) >= 0) root.style.fontSize = display.scale + "%";
  var dark =
    display.theme === "dark" ||
    (display.theme !== "light" && matchMedia("(prefers-color-scheme: dark)").matches);
  root.dataset.theme = dark ? "dark" : "light";
  if (dark) document.querySelector('meta[name="theme-color"]').content = "#121821";
})();
