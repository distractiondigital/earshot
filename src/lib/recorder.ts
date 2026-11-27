import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

export type SystemSource =
  | { kind: "none" }
  | { kind: "all" }
  | { kind: "app"; pid: number; name: string }
  | { kind: "output"; id: string; name: string }
  | { kind: "input"; id: string; name: string };

export interface RecorderState {
  status: "idle" | "recording" | "paused";
  meetingId: number | null;
  startedAt: number | null;
  elapsedMs: number;
  micSource: string | null;
  systemSource: string | null;
  /** The microphone being recorded (null = system default). */
  micDevice: string | null;
  /** The meeting-audio source being recorded. */
  system: SystemSource | null;
  warnings: string[];
  silence: Silence;
}

/** Tracks that have heard nothing in the 15 seconds since their source started. */
export interface Silence {
  mic: boolean;
  system: boolean;
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
  outputDevices: InputDevice[];
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
  language: string | null;
  /** Remote speakers by name (or "Speaker N"), most talkative first. */
  speakers: string[];
}

export interface Interrupted {
  meetingId: number;
  title: string;
  startedAt: number;
  stoppedAt: number;
}

/** The hotkey continues an interruption this recent instead of starting a new recording. */
export const QUICK_CONTINUE_MS = 15 * 60 * 1000;

export const recorder = {
  state: () => invoke<RecorderState>("recorder_state"),
  start: (options: { micDevice?: string | null; system?: SystemSource | null }) =>
    invoke<RecorderState>("start_recording", { options }),
  stop: () => invoke<number>("stop_recording"),
  resume: (id: number) => invoke<RecorderState>("resume_recording", { id }),
  interrupted: () => invoke<Interrupted | null>("interrupted_meeting"),
  pause: (paused: boolean) => invoke<RecorderState>("pause_recording", { paused }),
  /** Switches the microphone while recording (null = system default). */
  switchMic: (micDevice: string | null) =>
    invoke<RecorderState>("switch_recording_source", { kind: "mic", micDevice }),
  /** Switches the meeting audio while recording. */
  switchSystem: (system: SystemSource) =>
    invoke<RecorderState>("switch_recording_source", { kind: "system", system }),
  /** Shows the main window on the recorder. */
  showRecorder: () => invoke<void>("show_recorder"),
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
  micDevice: null,
  system: null,
  warnings: [],
  silence: { mic: false, system: false },
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

/** Which tracks are silent, kept in sync with backend events. */
export function useSilence() {
  const [silence, setSilence] = useState<Silence>(IDLE.silence);
  useEffect(() => {
    let alive = true;
    recorder.state().then((s) => alive && setSilence(s.silence));
    const un = listen<Silence>("recorder://silence", (e) => setSilence(e.payload));
    const unState = listen<RecorderState>("recorder://state", (e) => setSilence(e.payload.silence));
    return () => {
      alive = false;
      un.then((f) => f());
      unState.then((f) => f());
    };
  }, []);
  return silence;
}

/** What to tell the user about silent tracks, or null. `short` fits the floating pill. */
export function silenceMessage(s: Silence, short = false): string | null {
  if (s.mic && s.system) return short ? "Mic and meeting silent" : "No sound from your mic or the meeting yet";
  if (s.mic) return short ? "Mic is silent" : "No sound from your mic yet";
  if (s.system) return short ? "Meeting is silent" : "No sound from the meeting yet";
  return null;
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
