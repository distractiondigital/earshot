import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";

export interface Hardware {
  cpu: string;
  cores: number;
  ramBytes: number;
  nvidia: { name: string; vramBytes: number } | null;
  appleSilicon: boolean;
  workerGpu: string | null;
}

export interface ModelView {
  id: string;
  category: "transcription" | "vad" | "segmentation" | "embedding";
  name: string;
  description: string;
  sizeBytes: number;
  accuracy: number;
  speed: number;
  englishOnly: boolean;
  advanced: boolean;
  badge: string | null;
  license: string;
  state: "available" | "downloading" | "ready";
  downloadedBytes: number;
  selected: boolean;
  recommended: boolean;
}

export interface ModelsInfo {
  hardware: Hardware;
  recommended: string;
  models: ModelView[];
}

export interface Job {
  id: number;
  meetingId: number;
  kind: string;
  stage: string | null;
  progress: number;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  error: string | null;
}

export interface ProcessingProgress {
  meetingId: number;
  track: number;
  tracks: number;
  trackKind: string;
  stage: "decode" | "vad" | "load_model" | "transcribe" | "diarize";
  pct: number;
}

export interface Utterance {
  id: number;
  trackKind: "mic" | "system";
  speakerId: number | null;
  /** "Me" or "Speaker N"; null when the speaker isn't known. */
  speakerLabel: string | null;
  speakerName: string | null;
  isMe: boolean;
  /** Corrected by hand; word timings may not match the text any more. */
  edited: boolean;
  startMs: number;
  endMs: number;
  text: string;
  words: { startMs: number; endMs: number; text: string; prob: number | null }[];
}

export const models = {
  list: () => invoke<ModelsInfo>("list_models"),
  download: (id: string) => invoke<void>("download_model", { id }),
  cancelDownload: (id: string) => invoke<void>("cancel_download", { id }),
  remove: (id: string) => invoke<void>("delete_model", { id }),
  select: (id: string) => invoke<void>("select_model", { id }),
};

export const processing = {
  transcribe: (id: number, cpu = false, model: string | null = null) =>
    invoke<void>("transcribe_meeting", { id, cpu, model }),
  cancel: (id: number) => invoke<void>("cancel_processing", { id }),
  jobs: () => invoke<Job[]>("processing_jobs"),
  transcript: (id: number) => invoke<Utterance[]>("get_transcript", { id }),
};

/** Model catalog state, refreshed on download/selection changes; live download progress. */
export function useModels() {
  const [info, setInfo] = useState<ModelsInfo | null>(null);
  const [progress, setProgress] = useState<Record<string, number>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const refresh = useCallback(() => {
    models.list().then(setInfo).catch(() => {});
  }, []);
  useEffect(() => {
    refresh();
    const unChanged = listen<{ id: string; error: string | null }>("models://changed", (e) => {
      setErrors((prev) => {
        const next = { ...prev };
        if (e.payload.error) next[e.payload.id] = e.payload.error;
        else delete next[e.payload.id];
        return next;
      });
      refresh();
    });
    const unProgress = listen<{ id: string; downloaded: number }>("models://progress", (e) =>
      setProgress((p) => ({ ...p, [e.payload.id]: e.payload.downloaded })),
    );
    return () => {
      unChanged.then((f) => f());
      unProgress.then((f) => f());
    };
  }, [refresh]);
  return { info, progress, errors, refresh };
}

/** Latest job per meeting plus live progress of the running one. */
export function useProcessing() {
  const [jobs, setJobs] = useState<Record<number, Job>>({});
  const [live, setLive] = useState<ProcessingProgress | null>(null);
  const refresh = useCallback(() => {
    processing
      .jobs()
      .then((list) => setJobs(Object.fromEntries(list.map((j) => [j.meetingId, j]))))
      .catch(() => {});
  }, []);
  useEffect(() => {
    refresh();
    const unChanged = listen<number>("processing://changed", () => {
      setLive(null);
      refresh();
    });
    const unProgress = listen<ProcessingProgress>("processing://progress", (e) => setLive(e.payload));
    return () => {
      unChanged.then((f) => f());
      unProgress.then((f) => f());
    };
  }, [refresh]);
  return { jobs, live, refresh };
}

export function formatBytes(n: number) {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(1)} GB`;
  if (n >= 1024 ** 2) return `${Math.round(n / 1024 ** 2)} MB`;
  return `${Math.max(1, Math.round(n / 1024))} KB`;
}
