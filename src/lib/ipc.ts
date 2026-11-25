import { invoke } from "@tauri-apps/api/core";

export type Theme = "light" | "dark" | "system";

export interface AppInfo {
  version: string;
  schemaVersion: number;
  dataDir: string;
  workerVersion: string | null;
}

export const ipc = {
  getSetting: (key: string) => invoke<string | null>("get_setting", { key }),
  setSetting: (key: string, value: string) => invoke<void>("set_setting", { key, value }),
  appInfo: () => invoke<AppInfo>("app_info"),
};
