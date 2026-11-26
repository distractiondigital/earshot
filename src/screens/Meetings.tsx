import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, Circle, Headphones, Pause, Play, Search, Square, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { SearchResults } from "@/components/SearchResults";
import { Welcome } from "@/components/Welcome";
import { parseSavedSource, SourceSelect } from "@/components/SourceSelect";
import { processing, useModels, useProcessing, type Job } from "@/lib/models";
import { FOCUS_SEARCH } from "@/lib/events";
import { ipc } from "@/lib/ipc";
import { summaries } from "@/lib/summary";
import {
  formatDuration,
  formatElapsed,
  QUICK_CONTINUE_MS,
  recorder,
  useLevels,
  useRecorderState,
  type Interrupted,
  type Meeting,
  type Sources,
  type SystemSource,
} from "@/lib/recorder";

export function Meetings({
  onOpen,
  onOpenSettings,
}: {
  onOpen: (id: number, atMs?: number) => void;
  onOpenSettings: (tab: string) => void;
}) {
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const focus = () => searchRef.current?.focus();
    window.addEventListener(FOCUS_SEARCH, focus);
    return () => window.removeEventListener(FOCUS_SEARCH, focus);
  }, []);
  const state = useRecorderState();
  const { jobs, live, refresh: refreshJobs } = useProcessing();
  const { info: modelsInfo } = useModels();
  const noModel = modelsInfo !== null && !modelsInfo.models.some((m) => m.selected);
  const [meetings, setMeetings] = useState<Meeting[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [detailsFor, setDetailsFor] = useState<number | null>(null);
  const [interrupted, setInterrupted] = useState<Interrupted | null>(null);
  // Meetings whose summary is being written (or waiting to be).
  const [summarizing, setSummarizing] = useState<Set<number>>(new Set());

  const refresh = useCallback(() => {
    recorder.meetings().then(setMeetings).catch((e) => setError(String(e)));
    recorder.interrupted().then(setInterrupted).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    const unStopped = listen<number>("recorder://stopped", (e) => {
      refresh();
      setDetailsFor(e.payload);
    });
    const unState = listen("recorder://state", refresh);
    const unProc = listen("processing://changed", refresh);
    const loadSummaries = () =>
      summaries
        .jobs()
        .then((js) => setSummarizing(new Set(js.filter((j) => j.status === "running" || j.status === "queued").map((j) => j.meetingId))))
        .catch(() => {});
    loadSummaries();
    const unSummary = listen("summary://changed", () => {
      loadSummaries();
      refresh();
    });
    const unError = listen<string>("recorder://error", (e) => setError(e.payload));
    return () => {
      for (const u of [unStopped, unState, unError, unProc, unSummary]) u.then((f) => f());
    };
  }, [refresh]);

  const recording = state.status !== "idle";

  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col p-8">
      <h1 className="text-lg font-semibold">Meetings</h1>

      {error && (
        <div role="alert" className="mt-4 flex items-start gap-2 rounded-md border border-destructive/40 px-3 py-2 text-sm">
          <span className="flex-1 whitespace-pre-line text-destructive">{error}</span>
          <button type="button" aria-label="Dismiss" onClick={() => setError(null)} className="text-muted-foreground">
            <X className="size-4" />
          </button>
        </div>
      )}

      {!recording && interrupted && (
        <InterruptedBanner info={interrupted} onDone={refresh} onError={setError} />
      )}

      {recording ? (
        <RecordingCard />
      ) : (
        <RecordControls
          onError={setError}
          compact={meetings === null || meetings.length > 0}
          welcome={<Welcome onOpenSettings={onOpenSettings} />}
        />
      )}

      {recording && <HeadphonesTip />}

      {!recording && noModel && !!meetings?.length && (
        <div className="mt-4 flex items-center gap-3 rounded-md bg-muted/60 px-3 py-2 text-sm">
          <p className="flex-1 text-muted-foreground">
            Recordings are saved, but transcription isn't set up yet.
          </p>
          <Button size="sm" variant="outline" onClick={() => onOpenSettings("transcription")}>
            Choose a model
          </Button>
        </div>
      )}

      {meetings && meetings.length > 0 && (
        <div className="relative mt-6">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
          <Input
            ref={searchRef}
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search meetings and transcripts"
            aria-label="Search meetings and transcripts"
            className="pl-8"
          />
        </div>
      )}

      {meetings && query.trim() && <SearchResults query={query} meetings={meetings} onOpen={onOpen} />}

      {meetings && meetings.length > 0 && !query.trim() && (
        <ul className="mt-4 divide-y divide-border border-t border-border" aria-label="Meetings">
          {meetings
            .filter((m) => m.status !== "recording")
            .map((m) => (
              <MeetingRow
                key={m.id}
                meeting={m}
                job={jobs[m.id]}
                livePct={live?.meetingId === m.id ? live.pct : null}
                summarizing={summarizing.has(m.id)}
                onChanged={() => {
                  refresh();
                  refreshJobs();
                }}
                onOpen={() => onOpen(m.id)}
                onError={setError}
              />
            ))}
        </ul>
      )}

      {detailsFor !== null && meetings && (
        <DetailsDialog
          meeting={meetings.find((m) => m.id === detailsFor) ?? null}
          onClose={() => {
            setDetailsFor(null);
            refresh();
          }}
        />
      )}
    </div>
  );
}

/** Shown after a crash: continue the same meeting, or keep what was recorded. */
function InterruptedBanner({
  info,
  onDone,
  onError,
}: {
  info: Interrupted;
  onDone: () => void;
  onError: (e: string) => void;
}) {
  const [busy, setBusy] = useState<"continue" | "save" | null>(null);
  const [hotkey, setHotkey] = useState("");
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    recorder.getHotkey().then(setHotkey);
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);

  const run = async (kind: "continue" | "save") => {
    setBusy(kind);
    try {
      if (kind === "continue") await recorder.resume(info.meetingId);
      else await recorder.recover(info.meetingId);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(null);
      onDone();
    }
  };

  const stopped = new Date(info.stoppedAt);
  const mins = Math.max(0, Math.round((now - info.stoppedAt) / 60000));
  const ago = mins === 0 ? "just now" : mins === 1 ? "1 minute ago" : `${mins} minutes ago`;
  const recent = now - info.stoppedAt < QUICK_CONTINUE_MS;
  const shortcut = hotkey.replace("CommandOrControl", navigator.userAgent.includes("Mac") ? "Cmd" : "Ctrl");

  return (
    <div role="alert" className="mt-6 rounded-lg border border-amber-500/50 bg-amber-500/5 p-4">
      <div className="flex items-start gap-3">
        <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" aria-hidden />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">Recording stopped unexpectedly</p>
          <p className="mt-0.5 text-sm text-muted-foreground">
            {info.title || "Untitled meeting"} stopped at{" "}
            {stopped.toLocaleTimeString(undefined, { timeStyle: "short" })} ({ago}). Everything up to then is
            safe.
            {recent && shortcut && ` Press ${shortcut} to continue it.`}
          </p>
          <div className="mt-3 flex gap-2">
            <Button size="sm" onClick={() => run("continue")} disabled={busy !== null}>
              <Circle className="fill-current text-red-500" aria-hidden />
              {busy === "continue" ? "Continuing…" : "Continue recording"}
            </Button>
            <Button size="sm" variant="outline" onClick={() => run("save")} disabled={busy !== null}>
              {busy === "save" ? "Saving…" : "Save what was recorded"}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}


function RecordControls({
  onError,
  compact,
  welcome,
}: {
  onError: (e: string) => void;
  compact: boolean;
  welcome: React.ReactNode;
}) {
  const [sources, setSources] = useState<Sources | null>(null);
  const [source, setSource] = useState<SystemSource>({ kind: "all" });
  const [starting, setStarting] = useState(false);

  const loadSources = useCallback(() => {
    recorder.sources().then(setSources).catch(() => {});
  }, []);

  useEffect(() => {
    loadSources();
    ipc.getSetting("recording.systemSource").then((v) => setSource(parseSavedSource(v)));
  }, [loadSources]);

  const start = async () => {
    setStarting(true);
    try {
      await recorder.start({ system: source });
    } catch (e) {
      onError(String(e));
    } finally {
      setStarting(false);
    }
  };

  return (
    <div className={compact ? "mt-6 flex items-center gap-3" : "flex flex-1 flex-col items-center justify-center gap-6"}>
      {!compact && welcome}
      <div className={compact ? "flex items-center gap-2" : "flex w-full max-w-lg items-center gap-2 pl-8"}>
        <Button size="lg" onClick={start} disabled={starting}>
          <Circle className="fill-current text-red-500" aria-hidden />
          Record
        </Button>
        <SourceSelect value={source} onChange={setSource} sources={sources} onOpen={loadSources} />
      </div>
    </div>
  );
}

function RecordingCard() {
  const state = useRecorderState();
  const levels = useLevels();
  const paused = state.status === "paused";
  return (
    <div className="mt-6 rounded-lg border border-border p-4">
      <div className="flex items-center gap-3">
        <span aria-hidden className={`size-2.5 rounded-full bg-red-500 ${paused ? "" : "motion-safe:animate-pulse"}`} />
        <span className="text-sm font-medium">{paused ? "Paused" : "Recording"}</span>
        <span className="text-sm text-muted-foreground tabular-nums">
          {formatElapsed(levels?.elapsedMs ?? state.elapsedMs)}
        </span>
        <div className="ml-auto flex gap-2">
          <Button variant="outline" size="sm" onClick={() => recorder.pause(!paused)}>
            {paused ? <Play aria-hidden /> : <Pause aria-hidden />}
            {paused ? "Resume" : "Pause"}
          </Button>
          <Button size="sm" onClick={() => recorder.stop()}>
            <Square className="fill-current" aria-hidden />
            Stop
          </Button>
        </div>
      </div>
      <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Mic</dt>
        <dd>{state.micSource ?? "Not recorded"}</dd>
        <dt className="text-muted-foreground">Meeting</dt>
        <dd>{state.systemSource ?? "Not recorded"}</dd>
      </dl>
      {state.warnings.length > 0 && (
        <p className="mt-3 text-sm whitespace-pre-line text-destructive" role="alert">
          {state.warnings.join("\n")}
        </p>
      )}
    </div>
  );
}

/** One-time hint shown during the first recording. */
function HeadphonesTip() {
  const [show, setShow] = useState(false);
  useEffect(() => {
    ipc.getSetting("ui.headphonesTipShown").then((v) => v !== "true" && setShow(true));
  }, []);
  if (!show) return null;
  const dismiss = () => {
    setShow(false);
    ipc.setSetting("ui.headphonesTipShown", "true");
  };
  return (
    <div className="mt-3 flex items-start gap-3 rounded-md bg-muted/60 px-3 py-2 text-sm">
      <Headphones className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
      <p className="flex-1 text-muted-foreground">
        Tip: Use headphones to reduce feedback or echo from your speakers.
      </p>
      <button type="button" aria-label="Dismiss tip" onClick={dismiss} className="text-muted-foreground">
        <X className="size-4" />
      </button>
    </div>
  );
}

const STATUS: Record<Meeting["status"], string> = {
  recording: "Recording",
  incomplete: "Interrupted",
  queued: "Recorded",
  processing: "Processing",
  ready: "Ready",
  failed: "Failed",
};

function MeetingRow({
  meeting: m,
  job,
  livePct,
  summarizing,
  onChanged,
  onOpen,
  onError,
}: {
  meeting: Meeting;
  job: Job | undefined;
  livePct: number | null;
  summarizing: boolean;
  onChanged: () => void;
  onOpen: () => void;
  onError: (e: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const act = async (p: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await p();
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  const date = new Date(m.startedAt);
  const waiting = job?.status === "queued";
  const running = m.status === "processing" || job?.status === "running";
  const pct = Math.round(livePct ?? job?.progress ?? 0);
  return (
    <li className="flex items-center gap-4 py-3">
      <button type="button" onClick={onOpen} className="min-w-0 flex-1 text-left outline-none focus-visible:underline">
        <div className="truncate text-sm font-medium">{m.title || "Untitled meeting"}</div>
        <div className="text-xs text-muted-foreground">
          {date.toLocaleDateString(undefined, { dateStyle: "medium" })} ·{" "}
          {date.toLocaleTimeString(undefined, { timeStyle: "short" })}
          {m.durationMs > 0 && ` · ${formatDuration(m.durationMs)}`}
          {m.tags.length > 0 && ` · ${m.tags.join(", ")}`}
        </div>
        {m.speakers.length > 0 && (
          <div className="mt-1 flex flex-wrap gap-1" aria-label="Speakers">
            {m.speakers.slice(0, 4).map((s) => (
              <span key={s} className="rounded-full bg-muted px-2 py-0.5 text-xs text-muted-foreground">
                {s}
              </span>
            ))}
            {m.speakers.length > 4 && (
              <span className="px-1 py-0.5 text-xs text-muted-foreground">+{m.speakers.length - 4}</span>
            )}
          </div>
        )}
        {m.error && (
          <div
            className={`mt-1 text-xs whitespace-pre-line ${m.status === "failed" || m.status === "incomplete" ? "text-destructive" : "text-muted-foreground"}`}
          >
            {m.error}
          </div>
        )}
      </button>
      {m.status === "incomplete" ? (
        <Button size="sm" variant="outline" onClick={() => act(() => recorder.recover(m.id))} disabled={busy}>
          {busy ? "Recovering…" : "Recover"}
        </Button>
      ) : running || waiting ? (
        <div className="flex items-center gap-2">
          <span className="w-28 text-right text-xs text-muted-foreground tabular-nums" role="status">
            {running ? `Transcribing ${pct}%` : "Waiting…"}
          </span>
          <Button size="sm" variant="ghost" onClick={() => act(() => processing.cancel(m.id))} disabled={busy}>
            Cancel
          </Button>
        </div>
      ) : m.status === "failed" ? (
        <div className="flex gap-2">
          <Button size="sm" variant="outline" onClick={() => act(() => processing.transcribe(m.id))} disabled={busy}>
            Retry
          </Button>
          <Button size="sm" variant="ghost" onClick={() => act(() => processing.transcribe(m.id, true))} disabled={busy}>
            Retry on CPU
          </Button>
        </div>
      ) : m.status === "queued" ? (
        <Button size="sm" variant="outline" onClick={() => act(() => processing.transcribe(m.id))} disabled={busy}>
          Transcribe
        </Button>
      ) : summarizing && m.status === "ready" ? (
        <span className="text-xs text-muted-foreground" role="status">
          Summarizing…
        </span>
      ) : (
        <Badge variant="secondary">{STATUS[m.status]}</Badge>
      )}
    </li>
  );
}

/** Optional details collected after stopping (skippable). */
export function DetailsDialog({ meeting, onClose }: { meeting: Meeting | null; onClose: () => void }) {
  const [title, setTitle] = useState(meeting?.title ?? "");
  const [speakers, setSpeakers] = useState(meeting?.remoteSpeakerCountHint ? String(meeting.remoteSpeakerCountHint) : "auto");
  const [tags, setTags] = useState(meeting?.tags.join(", ") ?? "");
  const [error, setError] = useState<string | null>(null);
  if (!meeting) return null;

  const save = async () => {
    try {
      await recorder.updateMeeting(
        meeting.id,
        title,
        speakers === "auto" ? null : Number(speakers),
        tags.split(",").map((t) => t.trim()).filter(Boolean),
      );
      onClose();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Meeting details</DialogTitle>
          <DialogDescription>Optional. You can change these later.</DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="m-title">Title</Label>
            <Input id="m-title" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Untitled meeting" autoFocus />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="m-speakers">Other people on the call</Label>
            <Select value={speakers} onValueChange={setSpeakers}>
              <SelectTrigger id="m-speakers" className="w-40">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="auto">Auto-detect</SelectItem>
                {Array.from({ length: 10 }, (_, i) => (
                  <SelectItem key={i + 1} value={String(i + 1)}>
                    {i + 1}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="m-tags">Tags</Label>
            <Input id="m-tags" value={tags} onChange={(e) => setTags(e.target.value)} placeholder="client, weekly" />
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              Skip
            </Button>
            <Button type="submit">Save</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
