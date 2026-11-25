import { getCurrentWindow } from "@tauri-apps/api/window";
import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { ipc, type Theme } from "@/lib/ipc";

const CACHE_KEY = "earshot.theme";
const media = () => window.matchMedia("(prefers-color-scheme: dark)");

function isTheme(v: unknown): v is Theme {
  return v === "light" || v === "dark" || v === "system";
}

function apply(theme: Theme) {
  const dark = theme === "dark" || (theme === "system" && media().matches);
  document.documentElement.classList.toggle("dark", dark);
  // Keep native window chrome (title bar) in sync; null follows the OS.
  getCurrentWindow()
    .setTheme(theme === "system" ? null : theme)
    .catch(() => {});
}

interface ThemeCtx {
  theme: Theme;
  setTheme: (t: Theme) => void;
}

const Ctx = createContext<ThemeCtx | null>(null);

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(() => {
    const cached = localStorage.getItem(CACHE_KEY);
    return isTheme(cached) ? cached : "system";
  });

  // The database is the source of truth; localStorage only prevents a flash on startup.
  useEffect(() => {
    ipc
      .getSetting("theme")
      .then((v) => {
        if (isTheme(v)) setThemeState(v);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    apply(theme);
    localStorage.setItem(CACHE_KEY, theme);
    if (theme !== "system") return;
    const mq = media();
    const onChange = () => apply("system");
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [theme]);

  const setTheme = useCallback((t: Theme) => {
    setThemeState(t);
    ipc.setSetting("theme", t).catch((e) => console.error("failed to save theme", e));
  }, []);

  return <Ctx.Provider value={{ theme, setTheme }}>{children}</Ctx.Provider>;
}

export function useTheme() {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useTheme must be used inside ThemeProvider");
  return ctx;
}
