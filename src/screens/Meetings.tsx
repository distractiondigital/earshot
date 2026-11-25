import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Circle, Headphones, Pause, Play, Square, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectSeparator, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ipc } from "@/lib/ipc";
import {
  formatDuration,
  formatElapsed,
  recorder,
  useLevels,
  useRecorderState,
  type Meeting,
  type Sources,
  type SystemSource,
} from "@/lib/recorder";

export function Meetings() {
  const state = useRecorderState();
  const [meetings, setMeetings] = useState<Meeting[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [detailsFor, setDetailsFor] = useState<number | null>(null);

  const refresh = useCallback(() => {
    recorder.meetings().then(setMeetings).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
    const unStopped = listen<number>("recorder://stopped", (e) => {
      refresh();
      setDetailsFor(e.payload);
    });
    const unState = listen("recorder://state", refresh);
    const unError = listen<string>("recorder://error", (e) => setError(e.payload));
    return () => {
      for (const u of [unStopped, unState, unError]) u.then((f) => f());
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

      {recording ? (
        <RecordingCard />
      ) : (
        <RecordControls onError={setError} compact={!!meetings?.length} />
      )}

      {recording && <HeadphonesTip />}

      {meetings && meetings.length > 0 && (
        <ul className="mt-6 divide-y divide-border border-t border-border" aria-label="Meetings">
          {meetings
            .filter((m) => m.status !== "recording")
            .map((m) => (
              <MeetingRow key={m.id} meeting={m} onChanged={refresh} onEdit={() => setDetailsFor(m.id)} onError={setError} />
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

const ALL = "all";
const NONE = "none";

function RecordControls({ onError, compact }: { onError: (e: string) => void; compact: boolean }) {
  const [sources, setSources] = useState<Sources | null>(null);
  const [choice, setChoice] = useState<string>(ALL);
  const [starting, setStarting] = useState(false);

  const loadSources = useCallback(() => {
    recorder.sources().then(setSources).catch(() => {});
  }, []);

  useEffect(() => {
    loadSources();
    ipc.getSetting("recording.systemSource").then((v) => v === NONE && setChoice(NONE));
  }, [loadSources]);

  const start = async () => {
    let system: SystemSource = { kind: "all" };
    if (choice === NONE) system = { kind: "none" };
    else if (choice.startsWith("app:")) {
      const pid = Number(choice.slice(4));
      const app = sources?.apps.find((a) => a.pid === pid);
      if (app) system = { kind: "app", pid, name: app.name };
    }
    setStarting(true);
    try {
      await recorder.start({ system });
    } catch (e) {
      onError(String(e));
    } finally {
      setStarting(false);
    }
  };

  return (
    <div className={compact ? "mt-6 flex items-center gap-3" : "flex flex-1 flex-col items-center justify-center gap-4"}>
      {!compact && <p className="text-sm text-muted-foreground">No meetings yet.</p>}
      <div className="flex items-center gap-2">
        <Button size="lg" onClick={start} disabled={starting}>
          <Circle className="fill-current text-red-500" aria-hidden />
          Record
        </Button>
        <Select value={choice} onValueChange={setChoice} onOpenChange={(o) => o && loadSources()}>
          <SelectTrigger className="w-56" aria-label="Meeting audio source">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL}>All system audio</SelectItem>
            <SelectItem value={NONE}>Microphone only</SelectItem>
            {sources?.supportsAppCapture && sources.apps.length > 0 && (
              <>
                <SelectSeparator />
                <SelectGroup>
                  <SelectLabel>Only this app</SelectLabel>
                  {sources.apps.map((a) => (
                    <SelectItem key={a.pid} value={`app:${a.pid}`}>
                      {a.name}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </>
            )}
          </SelectContent>
        </Select>
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
        Tip: wear headphones. On speakers, other people's voices leak into your mic track.
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
  onChanged,
  onEdit,
  onError,
}: {
  meeting: Meeting;
  onChanged: () => void;
  onEdit: () => void;
  onError: (e: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const recover = async () => {
    setBusy(true);
    try {
      await recorder.recover(m.id);
    } catch (e) {
      onError(String(e));
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  const date = new Date(m.startedAt);
  return (
    <li className="flex items-center gap-4 py-3">
      <button type="button" onClick={onEdit} className="min-w-0 flex-1 text-left outline-none focus-visible:underline">
        <div className="truncate text-sm font-medium">{m.title || "Untitled meeting"}</div>
        <div className="text-xs text-muted-foreground">
          {date.toLocaleDateString(undefined, { dateStyle: "medium" })} ·{" "}
          {date.toLocaleTimeString(undefined, { timeStyle: "short" })}
          {m.durationMs > 0 && ` · ${formatDuration(m.durationMs)}`}
          {m.tags.length > 0 && ` · ${m.tags.join(", ")}`}
        </div>
        {m.error && <div className="mt-1 text-xs whitespace-pre-line text-destructive">{m.error}</div>}
      </button>
      {m.status === "incomplete" ? (
        <Button size="sm" variant="outline" onClick={recover} disabled={busy}>
          {busy ? "Recovering…" : "Recover"}
        </Button>
      ) : (
        <Badge variant={m.status === "failed" ? "destructive" : "secondary"}>{STATUS[m.status]}</Badge>
      )}
    </li>
  );
}

/** Optional details collected after stopping (skippable). */
function DetailsDialog({ meeting, onClose }: { meeting: Meeting | null; onClose: () => void }) {
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
