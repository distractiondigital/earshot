/**
 * Hides the webview's browser behaviour so the app feels like a desktop app:
 * no Back/Reload/Inspect context menu, and (in release builds) no reload, print or find shortcuts.
 * Text fields keep their menu so cut/copy/paste still work there.
 */
function isEditable(target: EventTarget | null) {
  const el = target instanceof Element ? target : null;
  return !!el?.closest("input, textarea, [contenteditable=''], [contenteditable='true']");
}

export function installNativeFeel() {
  window.addEventListener("contextmenu", (e) => {
    if (!isEditable(e.target)) e.preventDefault();
  });

  // Dev builds keep reload (handy while working); F12 / Ctrl+Shift+I still open dev tools there.
  if (import.meta.env.DEV) return;
  window.addEventListener("keydown", (e) => {
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key.toLowerCase();
    const blocked =
      e.key === "F5" ||
      e.key === "F3" ||
      e.key === "F7" ||
      (mod && ["r", "p", "f", "g", "u", "j"].includes(key)) ||
      (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) ||
      e.key === "BrowserBack" ||
      e.key === "BrowserForward" ||
      e.key === "BrowserRefresh";
    if (blocked) e.preventDefault();
  });
}
