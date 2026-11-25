import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ArrowLeft, Pencil, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { ipc } from "@/lib/ipc";
import { processing, useProcessing, type Utterance } from "@/lib/models";
import { formatDuration, recorder, type Meeting } from "@/lib/recorder";
import { DetailsDialog } from "@/screens/Meetings";
import { DEFAULT_ME_NAME } from "@/screens/SpeakersSettings";

/** Distinct, readable label colours for remote speakers (light and dark themes). */
const SPEAKER_COLORS = [
  "text-sky-700 dark:text-sky-300",
  "text-amber-700 dark:text-amber-300",
  "text-emerald-700 dark:text-emerald-300",
  "text-rose-700 dark:text-rose-300",
  "text-violet-700 dark:text-violet-300",
  "text-teal-700 dark:text-teal-300",
  "text-orange-700 dark:text-orange-300",
  "text-fuchsia-700 dark:text-fuchsia-300",
];

function speakerColor(u: Utterance) {
  if (u.isMe) return "text-foreground";
  const n = Number(/(\d+)$/.exec(u.speakerLabel ?? "")?.[1]);
  return Number.isFinite(n) && n > 0 ? SPEAKER_COLORS[(n - 1) % SPEAKER_COLORS.length] : "text-muted-foreground";
}

function speakerName(u: Utterance, meName: string) {
  if (u.isMe) return meName;
  return u.speakerName ?? u.speakerLabel ?? (u.trackKind === "mic" ? meName : "Meeting");
}

function stamp(ms: number) {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** Read-only transcript of one meeting (the full player arrives with M4). */
export function MeetingView({ id, onBack }: { id: number; onBack: () => void }) {
  const [meeting, setMeeting] = useState<Meeting | null>(null);
  const [lines, setLines] = useState<Utterance[] | null>(null);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [meName, setMeName] = useState(DEFAULT_ME_NAME);
  const { jobs, live } = useProcessing();

  useEffect(() => {
    ipc.getSetting("speakers.meName").then((v) => v && setMeName(v));
  }, []);

  const load = useCallback(() => {
    recorder
      .meetings()
      .then((all) => setMeeting(all.find((m) => m.id === id) ?? null))
      .catch((e) => setError(String(e)));
    processing.transcript(id).then(setLines).catch((e) => setError(String(e)));
  }, [id]);

  useEffect(() => {
    load();
    const un = listen<number>("processing://changed", (e) => e.payload === id && load());
    return () => {
      un.then((f) => f());
    };
  }, [id, load]);

  const job = jobs[id];
  const running = meeting?.status === "processing" || job?.status === "running" || job?.status === "queued";
  const pct = live?.meetingId === id ? live.pct : (job?.progress ?? 0);
  const date = meeting ? new Date(meeting.startedAt) : null;

  return (
    <div className="mx-auto max-w-3xl p-8">
      <Button variant="ghost" size="sm" onClick={onBack} className="-ml-2">
        <ArrowLeft aria-hidden />
        Meetings
      </Button>

      {meeting && date && (
        <header className="mt-3 flex items-start gap-4">
          <div className="min-w-0 flex-1">
            <h1 className="truncate text-lg font-semibold">{meeting.title || "Untitled meeting"}</h1>
            <p className="text-sm text-muted-foreground">
              {date.toLocaleDateString(undefined, { dateStyle: "medium" })} ·{" "}
              {date.toLocaleTimeString(undefined, { timeStyle: "short" })}
              {meeting.durationMs > 0 && ` · ${formatDuration(meeting.durationMs)}`}
            </p>
          </div>
          <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
            <Pencil aria-hidden />
            Edit details
          </Button>
          {!running && meeting.status !== "incomplete" && (
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                processing
                  .transcribe(id)
                  .then(load)
                  .catch((e) => setError(String(e)))
              }
            >
              <RotateCw aria-hidden />
              {lines?.length ? "Transcribe again" : "Transcribe"}
            </Button>
          )}
        </header>
      )}

      {error && (
        <p className="mt-4 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}

      {running && (
        <div className="mt-6 flex items-center gap-3">
          <Progress value={pct} className="flex-1" aria-label="Transcription progress" />
          <span className="w-24 text-right text-sm text-muted-foreground tabular-nums">
            {job?.status === "queued" ? "Waiting…" : `${Math.round(pct)}%`}
          </span>
        </div>
      )}

      {meeting?.error && <p className="mt-4 text-sm text-muted-foreground">{meeting.error}</p>}

      {lines && lines.length === 0 && !running && (
        <p className="mt-8 text-sm text-muted-foreground">No transcript yet.</p>
      )}

      {lines && lines.length > 0 && (
        <ol className="mt-6 space-y-3 select-text" aria-label="Transcript">
          {lines.map((u) => (
            <li key={u.id} className="grid grid-cols-[3.5rem_7rem_1fr] gap-2 text-sm">
              <span className="text-muted-foreground tabular-nums">{stamp(u.startMs)}</span>
              <span className={`truncate font-medium ${speakerColor(u)}`} title={speakerName(u, meName)}>
                {speakerName(u, meName)}
              </span>
              <span>{u.text}</span>
            </li>
          ))}
        </ol>
      )}

      {editing && meeting && (
        <DetailsDialog
          meeting={meeting}
          onClose={() => {
            setEditing(false);
            load();
          }}
        />
      )}
    </div>
  );
}
