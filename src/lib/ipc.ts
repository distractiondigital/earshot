import { invoke } from "@tauri-apps/api/core";

export type Theme = "light" | "dark" | "system";

export interface AppInfo {
  version: string;
  schemaVersion: number;
  dataDir: string;
  workerVersion: string | null;
  /** "CUDA" for the NVIDIA build, null for the standard one. */
  flavor: string | null;
  updatesEnabled: boolean;
}

export const ipc = {
  getSetting: (key: string) => invoke<string | null>("get_setting", { key }),
  setSetting: (key: string, value: string) => invoke<void>("set_setting", { key, value }),
  appInfo: () => invoke<AppInfo>("app_info"),
  openLink: (url: string) => invoke<void>("open_link", { url }),
  revealLicenses: () => invoke<void>("reveal_licenses"),
};
