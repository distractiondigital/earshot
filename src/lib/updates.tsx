import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { ipc } from "@/lib/ipc";

// Update flow: check (GitHub-hosted latest.json) -> prompt -> download + verify signature -> install -> restart.
// The download is verified against the public key embedded in tauri.conf.json before anything is installed.

export type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "upToDate" }
  | { kind: "available"; update: Update }
  | { kind: "downloading"; update: Update; downloaded: number; total: number | null }
  | { kind: "installing"; update: Update }
  | { kind: "error"; message: string };

interface UpdatesCtx {
  /** False for dev builds; the updater is never used there. */
  enabled: boolean;
  autoCheck: boolean;
  setAutoCheck: (v: boolean) => void;
  status: UpdateStatus;
  /** True when the banner was dismissed for the update currently offered. */
  dismissed: boolean;
  checkNow: () => Promise<void>;
  install: () => Promise<void>;
  dismiss: () => void;
}

const Ctx = createContext<UpdatesCtx | null>(null);
const AUTO_CHECK_KEY = "updates.autoCheck";

export function UpdatesProvider({ children }: { children: ReactNode }) {
  const [enabled, setEnabled] = useState(false);
  const [autoCheck, setAutoCheckState] = useState(true);
  const [status, setStatus] = useState<UpdateStatus>({ kind: "idle" });
  const [dismissed, setDismissed] = useState(false);
  const busy = useRef(false);

  const checkNow = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    setStatus({ kind: "checking" });
    try {
      const update = await check();
      setDismissed(false);
      setStatus(update ? { kind: "available", update } : { kind: "upToDate" });
    } catch (e) {
      setStatus({ kind: "error", message: String(e) });
    } finally {
      busy.current = false;
    }
  }, []);

  // One quiet check at launch, unless this is a dev build or the user turned it off.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [info, auto] = await Promise.all([ipc.appInfo(), ipc.getSetting(AUTO_CHECK_KEY)]);
      if (cancelled) return;
      const on = auto !== "false";
      setEnabled(info.updatesEnabled);
      setAutoCheckState(on);
      if (info.updatesEnabled && on) {
        await checkNow();
        // A failed background check is not worth interrupting anyone; Settings shows it on demand.
        setStatus((s) => (s.kind === "error" || s.kind === "upToDate" ? { kind: "idle" } : s));
      }
    })().catch((e) => console.error("update check setup failed", e));
    return () => {
      cancelled = true;
    };
  }, [checkNow]);

  const setAutoCheck = useCallback((v: boolean) => {
    setAutoCheckState(v);
    ipc.setSetting(AUTO_CHECK_KEY, String(v)).catch((e) => console.error("failed to save setting", e));
  }, []);

  const install = useCallback(async () => {
    if (status.kind !== "available" || busy.current) return;
    // TODO(M1): refuse while a recording is in progress.
    busy.current = true;
    const update = status.update;
    let downloaded = 0;
    let total: number | null = null;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
          setStatus({ kind: "downloading", update, downloaded: 0, total });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setStatus({ kind: "downloading", update, downloaded, total });
        } else if (event.event === "Finished") {
          setStatus({ kind: "installing", update });
        }
      });
      // On Windows the installer exits the app itself; elsewhere restart into the new version.
      await relaunch();
    } catch (e) {
      setStatus({ kind: "error", message: String(e) });
    } finally {
      busy.current = false;
    }
  }, [status]);

  const dismiss = useCallback(() => setDismissed(true), []);

  return (
    <Ctx.Provider value={{ enabled, autoCheck, setAutoCheck, status, dismissed, checkNow, install, dismiss }}>
      {children}
    </Ctx.Provider>
  );
}

export function useUpdates() {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useUpdates must be used inside UpdatesProvider");
  return ctx;
}
