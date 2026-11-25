import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

export type SystemSource = { kind: "none" } | { kind: "all" } | { kind: "app"; pid: number; name: string };

export interface RecorderState {
  status: "idle" | "recording" | "paused";
  meetingId: number | null;
  startedAt: number | null;
  elapsedMs: number;
  micSource: string | null;
  systemSource: string | null;
  warnings: string[];
}

export interface Levels {
  mic: number | null;
  system: number | null;
  elapsedMs: number;
  micFailed: boolean;
  systemFailed: boolean;
}

export interface InputDevice {
  id: string;
  name: string;
  isDefault: boolean;
}

export interface AudioApp {
  pid: number;
  name: string;
  active: boolean;
}

export interface Sources {
  inputDevices: InputDevice[];
  apps: AudioApp[];
  supportsAppCapture: boolean;
}

export interface Meeting {
  id: number;
  title: string;
  startedAt: number;
  durationMs: number;
  status: "recording" | "incomplete" | "queued" | "processing" | "ready" | "failed";
  error: string | null;
  remoteSpeakerCountHint: number | null;
  tags: string[];
}

export const recorder = {
  state: () => invoke<RecorderState>("recorder_state"),
  start: (options: { micDevice?: string | null; system?: SystemSource | null }) =>
    invoke<RecorderState>("start_recording", { options }),
  stop: () => invoke<number>("stop_recording"),
  pause: (paused: boolean) => invoke<RecorderState>("pause_recording", { paused }),
  sources: () => invoke<Sources>("list_sources"),
  meetings: () => invoke<Meeting[]>("list_meetings"),
  updateMeeting: (id: number, title: string, speakerHint: number | null, tags: string[]) =>
    invoke<void>("update_meeting", { id, title, speakerHint, tags }),
  recover: (id: number) => invoke<number>("recover_meeting", { id }),
  recordingsDir: () => invoke<string>("recordings_dir"),
  chooseRecordingsDir: () => invoke<string | null>("choose_recordings_dir"),
  openRecordingsDir: () => invoke<void>("open_recordings_dir"),
  getHotkey: () => invoke<string>("get_hotkey"),
  setHotkey: (hotkey: string) => invoke<void>("set_hotkey", { hotkey }),
};

const IDLE: RecorderState = {
  status: "idle",
  meetingId: null,
  startedAt: null,
  elapsedMs: 0,
  micSource: null,
  systemSource: null,
  warnings: [],
};

/** Live recorder state, kept in sync with backend events. */
export function useRecorderState() {
  const [state, setState] = useState<RecorderState>(IDLE);
  useEffect(() => {
    let alive = true;
    recorder.state().then((s) => alive && setState(s));
    const un = listen<RecorderState>("recorder://state", (e) => setState(e.payload));
    return () => {
      alive = false;
      un.then((f) => f());
    };
  }, []);
  return state;
}

/** Live level meters (~15 Hz while recording). */
export function useLevels() {
  const [levels, setLevels] = useState<Levels | null>(null);
  useEffect(() => {
    const un = listen<Levels>("recorder://levels", (e) => setLevels(e.payload));
    const unState = listen<RecorderState>("recorder://state", (e) => {
      if (e.payload.status === "idle") setLevels(null);
    });
    return () => {
      un.then((f) => f());
      unState.then((f) => f());
    };
  }, []);
  return levels;
}

export function formatElapsed(ms: number) {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const mm = String(m).padStart(h ? 2 : 1, "0");
  return `${h ? `${h}:` : ""}${mm}:${String(sec).padStart(2, "0")}`;
}

export function formatDuration(ms: number) {
  const mins = Math.round(ms / 60000);
  if (mins < 1) return `${Math.max(1, Math.round(ms / 1000))} s`;
  if (mins < 60) return `${mins} min`;
  return `${Math.floor(mins / 60)} h ${mins % 60} min`;
}
