import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import type { Job } from "@/lib/models";

export interface Template {
  id: number;
  name: string;
  instructions: string;
  sections: string[];
  builtin: boolean;
}

export interface Summary {
  markdown: string;
  templateId: number | null;
  templateName: string;
  modelId: string;
  createdAt: number;
  edited: boolean;
}

export interface SummaryState {
  summary: Summary | null;
  job: Job | null;
}

export type SummaryStage = "load_model" | "notes" | "summarize" | "title";
export type SummaryMode = "never" | "manual" | "auto";

export const summaries = {
  get: (id: number) => invoke<SummaryState>("get_summary", { id }),
  jobs: () => invoke<Job[]>("summary_jobs"),
  summarize: (id: number, templateId: number | null, model: string | null) =>
    invoke<void>("summarize_meeting", { id, templateId, model }),
  cancel: (id: number) => invoke<void>("cancel_summary", { id }),
  edit: (id: number, markdown: string) => invoke<void>("edit_summary", { id, markdown }),
  templates: () => invoke<Template[]>("list_templates"),
  saveTemplate: (t: { id: number | null; name: string; instructions: string; sections: string[] }) =>
    invoke<number>("save_template", t),
  deleteTemplate: (id: number) => invoke<void>("delete_template", { id }),
  resetTemplate: (id: number) => invoke<void>("reset_template", { id }),
};

/** A meeting's summary, plus the text being written right now. */
export function useSummary(id: number) {
  const [state, setState] = useState<SummaryState | null>(null);
  const [live, setLive] = useState("");
  const [stage, setStage] = useState<{ stage: SummaryStage; pct: number } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    summaries
      .get(id)
      .then((s) => {
        setState(s);
        setError(null);
        if (s.job?.status !== "running") {
          setLive("");
          setStage(null);
        }
      })
      .catch((e) => setError(String(e)));
  }, [id]);

  useEffect(() => {
    refresh();
    const subs = [
      listen<number>("summary://changed", (e) => e.payload === id && refresh()),
      listen<{ meetingId: number; text: string }>("summary://token", (e) => {
        if (e.payload.meetingId === id) setLive((t) => t + e.payload.text);
      }),
      listen<{ meetingId: number; stage: SummaryStage; pct: number }>("summary://progress", (e) => {
        if (e.payload.meetingId !== id) return;
        setStage({ stage: e.payload.stage, pct: e.payload.pct });
        // A new attempt (e.g. retried on the CPU) starts its text again.
        if (e.payload.stage === "load_model" && e.payload.pct === 0) setLive("");
      }),
    ];
    return () => {
      subs.forEach((u) => u.then((f) => f()));
    };
  }, [id, refresh]);

  return { state, live, stage, error, refresh };
}

export const STAGE_LABEL: Record<SummaryStage, string> = {
  load_model: "Loading the model…",
  notes: "Reading the meeting…",
  summarize: "Writing…",
  title: "Naming the meeting…",
};
