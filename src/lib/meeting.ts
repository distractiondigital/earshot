import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

export interface TrackAudio {
  kind: "mic" | "system";
  path: string;
  durationMs: number;
  source: string | null;
}

export interface Speaker {
  id: number;
  /** "Me" or "Speaker N". */
  label: string;
  /** Name given by the user. */
  name: string | null;
  isMe: boolean;
  talkMs: number;
  utterances: number;
  /** Linked voice profile. */
  profileId: number | null;
  profileName: string | null;
  /** The profile was suggested by voice matching and isn't confirmed yet. */
  autoMatched: boolean;
  /** Enough clear speech was found to save this voice. */
  hasVoice: boolean;
}

export interface Profile {
  id: number;
  name: string;
  createdAt: number;
  meetings: number;
  lastSeen: number | null;
  /** [voice model id, sample count] */
  samples: [string, number][];
}

export interface ProfileMeeting {
  meetingId: number;
  title: string;
  startedAt: number;
  autoMatched: boolean;
}

export const profiles = {
  list: () => invoke<Profile[]>("list_profiles"),
  meetings: (id: number) => invoke<ProfileMeeting[]>("profile_meetings", { id }),
  save: (speakerId: number, name: string) => invoke<number>("save_voice_profile", { speakerId, name }),
  link: (speakerId: number, profileId: number) => invoke<void>("link_voice_profile", { speakerId, profileId }),
  unlink: (speakerId: number) => invoke<void>("unlink_voice_profile", { speakerId }),
  rename: (id: number, name: string) => invoke<void>("rename_profile", { id, name }),
  remove: (id: number) => invoke<void>("delete_profile", { id }),
};

export interface SearchHit {
  meetingId: number;
  utteranceId: number;
  startMs: number;
  /** Matching text; matches are wrapped in  … . */
  snippet: string;
}

export type ExportFormat = "md" | "txt" | "srt" | "vtt" | "json";

/** Local date (YYYY-MM-DD) and a readable date + time for exports. */
function exportDates(startedAt: number) {
  const d = new Date(startedAt);
  const pad = (n: number) => String(n).padStart(2, "0");
  return {
    date: `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`,
    when: d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }),
  };
}

/** "tracks": each track as recorded (FLAC); "flac" / "mp3": one file with the tracks mixed. */
export type AudioExport = "tracks" | "flac" | "mp3";

/** Emitted with the meeting id once a mixed export starts writing (after the save dialog). */
export const AUDIO_MIX_STARTED = "audio-mix-started";

export const meeting = {
  search: (query: string) => invoke<SearchHit[]>("search_transcripts", { query }),
  exportTranscript: (id: number, format: ExportFormat, startedAt: number) =>
    invoke<string | null>("export_meeting", { id, format, ...exportDates(startedAt) }),
  exportAudio: (id: number, kind: AudioExport, startedAt: number) =>
    invoke<string | null>("export_audio", { id, kind, date: exportDates(startedAt).date }),
  remove: (id: number) => invoke<void>("delete_meeting", { id }),
  audio: (id: number) => invoke<TrackAudio[]>("meeting_audio", { id }),
  speakers: (id: number) => invoke<Speaker[]>("get_speakers", { id }),
  renameSpeaker: (id: number, name: string) => invoke<void>("rename_speaker", { id, name }),
  mergeSpeakers: (from: number, into: number) => invoke<void>("merge_speakers", { from, into }),
  setUtteranceSpeaker: (id: number, speakerId: number) => invoke<void>("set_utterance_speaker", { id, speakerId }),
  setUtteranceText: (id: number, text: string) => invoke<void>("set_utterance_text", { id, text }),
  separateSpeakers: (id: number, speakerCount: number | null, sensitivity: number | null) =>
    invoke<void>("separate_speakers", { id, speakerCount, sensitivity }),
};

/** Distinct, readable label colours for remote speakers (light and dark themes). */
const SPEAKER_COLORS = [
  { text: "text-sky-700 dark:text-sky-300", bg: "bg-sky-500" },
  { text: "text-amber-700 dark:text-amber-300", bg: "bg-amber-500" },
  { text: "text-emerald-700 dark:text-emerald-300", bg: "bg-emerald-500" },
  { text: "text-rose-700 dark:text-rose-300", bg: "bg-rose-500" },
  { text: "text-violet-700 dark:text-violet-300", bg: "bg-violet-500" },
  { text: "text-teal-700 dark:text-teal-300", bg: "bg-teal-500" },
  { text: "text-orange-700 dark:text-orange-300", bg: "bg-orange-500" },
  { text: "text-fuchsia-700 dark:text-fuchsia-300", bg: "bg-fuchsia-500" },
];
const ME_COLOR = { text: "text-foreground", bg: "bg-foreground" };
const UNKNOWN_COLOR = { text: "text-muted-foreground", bg: "bg-muted-foreground" };

/** Colour for a speaker by its label ("Speaker 3" -> third colour); stable across renames. */
export function speakerColor(label: string | null, isMe: boolean) {
  if (isMe) return ME_COLOR;
  const n = Number(/(\d+)$/.exec(label ?? "")?.[1]);
  return Number.isFinite(n) && n > 0 ? SPEAKER_COLORS[(n - 1) % SPEAKER_COLORS.length] : UNKNOWN_COLOR;
}

export function speakerDisplayName(
  s: { label: string | null; name: string | null; isMe: boolean; profileName?: string | null },
  meName: string,
  fallback = "Meeting",
) {
  if (s.isMe) return meName;
  return s.name ?? s.profileName ?? s.label ?? fallback;
}

/** m:ss or h:mm:ss. */
export function clock(ms: number) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** Tracks drift apart by more than this before the follower is nudged back. */
const MAX_DRIFT_S = 0.08;

export type TimeListener = (ms: number) => void;

/**
 * Plays a meeting's tracks (mic + meeting audio) together. The longest track is the clock; the
 * others follow it and are corrected when they drift. Each track can be muted.
 */
export function useSyncedPlayer(tracks: TrackAudio[]) {
  const els = useRef(new Map<string, HTMLAudioElement>());
  const listeners = useRef(new Set<TimeListener>());
  const playingRef = useRef(false);
  const [playing, setPlaying] = useState(false);
  const [timeMs, setTimeMs] = useState(0);
  const [rate, setRateState] = useState(1);
  const [muted, setMuted] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);
  const durationMs = useMemo(() => Math.max(0, ...tracks.map((t) => t.durationMs)), [tracks]);
  const masterKind = useMemo(
    () => [...tracks].sort((a, b) => b.durationMs - a.durationMs)[0]?.kind ?? null,
    [tracks],
  );

  const emit = useCallback((ms: number) => {
    for (const fn of listeners.current) fn(ms);
  }, []);

  // Create one audio element per track.
  useEffect(() => {
    const map = new Map<string, HTMLAudioElement>();
    for (const t of tracks) {
      const a = new Audio();
      a.preload = "auto";
      a.src = convertFileSrc(t.path);
      a.onerror = () => setError("Couldn't play this recording.");
      map.set(t.kind, a);
    }
    els.current = map;
    return () => {
      for (const a of map.values()) {
        a.pause();
        a.removeAttribute("src");
        a.load();
      }
      playingRef.current = false;
      setPlaying(false);
    };
  }, [tracks]);

  // Keep followers in step with the master while playing, and publish the time.
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    let lastUi = 0;
    const tick = (now: number) => {
      const master = masterKind ? els.current.get(masterKind) : undefined;
      if (master) {
        const t = master.currentTime;
        for (const [kind, a] of els.current) {
          if (kind === masterKind) continue;
          const within = Number.isFinite(a.duration) ? t < a.duration - 0.05 : true;
          if (within && a.paused) a.play().catch(() => {});
          if (within && Math.abs(a.currentTime - t) > MAX_DRIFT_S) a.currentTime = t;
        }
        emit(t * 1000);
        if (now - lastUi > 100) {
          lastUi = now;
          setTimeMs(t * 1000);
        }
        if (master.ended) {
          playingRef.current = false;
          setPlaying(false);
          for (const a of els.current.values()) a.pause();
          return;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, masterKind, emit]);

  const play = useCallback(async () => {
    const master = masterKind ? els.current.get(masterKind) : undefined;
    if (!master) return;
    if (master.ended) {
      for (const a of els.current.values()) a.currentTime = 0;
    }
    const t = master.currentTime;
    try {
      await Promise.all(
        [...els.current.entries()].map(([kind, a]) => {
          if (kind !== masterKind && Number.isFinite(a.duration) && t >= a.duration) return null;
          return a.play();
        }),
      );
      playingRef.current = true;
      setPlaying(true);
      setError(null);
    } catch (e) {
      setError(`Couldn't play this recording: ${e}`);
    }
  }, [masterKind]);

  const pause = useCallback(() => {
    for (const a of els.current.values()) a.pause();
    playingRef.current = false;
    setPlaying(false);
  }, []);

  const seek = useCallback(
    (ms: number) => {
      const s = Math.min(Math.max(0, ms), durationMs) / 1000;
      for (const a of els.current.values()) {
        const end = Number.isFinite(a.duration) ? a.duration : Infinity;
        a.currentTime = Math.min(s, end);
      }
      setTimeMs(s * 1000);
      emit(s * 1000);
    },
    [durationMs, emit],
  );

  const setRate = useCallback((r: number) => {
    for (const a of els.current.values()) a.playbackRate = r;
    setRateState(r);
  }, []);

  const toggleMute = useCallback((kind: string) => {
    setMuted((m) => {
      const next = { ...m, [kind]: !m[kind] };
      const a = els.current.get(kind);
      if (a) a.muted = next[kind];
      return next;
    });
  }, []);

  const subscribe = useCallback((fn: TimeListener) => {
    listeners.current.add(fn);
    return () => {
      listeners.current.delete(fn);
    };
  }, []);

  return { playing, timeMs, durationMs, rate, muted, error, play, pause, seek, setRate, toggleMute, subscribe };
}

export type Player = ReturnType<typeof useSyncedPlayer>;
