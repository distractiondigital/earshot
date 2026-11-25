// Applies the cached theme before first paint to avoid a flash. Kept as an external file
// so the CSP can forbid inline scripts.
(function () {
  var t = localStorage.getItem("earshot.theme") || "system";
  var dark = t === "dark" || (t === "system" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
})();
