import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ArrowLeft, Pencil } from "lucide-react";
import { MeetingMenu } from "@/components/MeetingMenu";
import { PlayerBar } from "@/components/PlayerBar";
import { RerunMenu } from "@/components/RerunMenu";
import { SpeakersPanel } from "@/components/SpeakersPanel";
import { SummaryPanel } from "@/components/SummaryPanel";
import { Transcript } from "@/components/Transcript";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ipc } from "@/lib/ipc";
import { meeting as meetingApi, useSyncedPlayer, type Speaker, type TrackAudio } from "@/lib/meeting";
import { processing, useProcessing, type Utterance } from "@/lib/models";
import { formatDuration, recorder, type Meeting } from "@/lib/recorder";
import { DetailsDialog } from "@/screens/Meetings";
import { DEFAULT_ME_NAME } from "@/screens/SpeakersSettings";

const NO_TRACKS: TrackAudio[] = [];

/** True when a key press belongs to a text field or control rather than the page. */
function typingInto(target: EventTarget | null) {
  const el = target instanceof HTMLElement ? target : null;
  return !!el?.closest("input, textarea, select, [contenteditable='true'], [role='slider'], [role='menu'], [role='dialog']");
}

/** One meeting: player, transcript with speakers, and editing. */
export function MeetingView({
  id,
  onBack,
  startAtMs,
  onOpenSettings,
}: {
  id: number;
  onBack: () => void;
  startAtMs?: number;
  /** Opens a Settings tab. */
  onOpenSettings: (tab: string) => void;
}) {
  const [meeting, setMeeting] = useState<Meeting | null>(null);
  const [lines, setLines] = useState<Utterance[] | null>(null);
  const [speakers, setSpeakers] = useState<Speaker[]>([]);
  const [tracks, setTracks] = useState<TrackAudio[]>(NO_TRACKS);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [meName, setMeName] = useState(DEFAULT_ME_NAME);
  const [summariesOn, setSummariesOn] = useState(true);
  const [tab, setTab] = useState("transcript");
  const { jobs, live } = useProcessing();
  const player = useSyncedPlayer(tracks);

  useEffect(() => {
    ipc.getSetting("speakers.meName").then((v) => v && setMeName(v));
    ipc.getSetting("summary.mode").then((v) => setSummariesOn(v !== "never"));
  }, []);

  const loadTranscript = useCallback(() => {
    processing.transcript(id).then(setLines).catch((e) => setError(String(e)));
    meetingApi.speakers(id).then(setSpeakers).catch((e) => setError(String(e)));
  }, [id]);

  const load = useCallback(() => {
    recorder
      .meetings()
      .then((all) => setMeeting(all.find((m) => m.id === id) ?? null))
      .catch((e) => setError(String(e)));
    loadTranscript();
  }, [id, loadTranscript]);

  useEffect(() => {
    load();
    meetingApi.audio(id).then(setTracks).catch((e) => setError(String(e)));
    // A summary job can also give the meeting its title.
    const subs = [
      listen<number>("processing://changed", (e) => e.payload === id && load()),
      listen<number>("summary://changed", (e) => e.payload === id && load()),
    ];
    return () => {
      subs.forEach((u) => u.then((f) => f()));
    };
  }, [id, load]);

  // Opened from a search result: jump to that moment once both the audio and the transcript are
  // loaded (the transcript scrolls to the line when the player reports the new time).
  const hasLines = !!lines?.length;
  useEffect(() => {
    if (startAtMs != null && tracks.length && hasLines) player.seek(startAtMs);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [startAtMs, tracks, hasLines]);

  // Space plays/pauses; arrows skip 5 s (10 s with Shift).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (typingInto(e.target) || e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === " " && !(e.target instanceof HTMLButtonElement)) {
        e.preventDefault();
        if (player.playing) player.pause();
        else player.play();
      } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.preventDefault();
        const step = (e.shiftKey ? 10_000 : 5_000) * (e.key === "ArrowLeft" ? -1 : 1);
        player.seek(player.timeMs + step);
      } else if (e.key === "Escape" && !e.defaultPrevented) {
        // Esc closes an open menu, popover or dialog first; only then does it leave the meeting.
        const overlay = document.querySelector('[role="dialog"], [role="menu"], [role="listbox"], [data-radix-popper-content-wrapper]');
        if (!overlay) onBack();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [player, onBack]);

  const job = jobs[id];
  const running = meeting?.status === "processing" || job?.status === "running" || job?.status === "queued";
  const pct = live?.meetingId === id ? live.pct : (job?.progress ?? 0);
  const date = meeting ? new Date(meeting.startedAt) : null;
  const onError = (e: string) => setError(e);

  return (
    <div className="mx-auto max-w-5xl px-8 pb-8">
      <div className="pt-8">
        <Button variant="ghost" size="sm" onClick={onBack} className="-ml-2">
          <ArrowLeft aria-hidden />
          Meetings
        </Button>
      </div>

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
            <RerunMenu
              id={id}
              hasTranscript={!!lines?.length}
              hasEdits={!!lines?.some((u) => u.edited)}
              speakerHint={meeting.remoteSpeakerCountHint}
              onQueued={load}
              onError={onError}
            />
          )}
          <MeetingMenu
            meeting={meeting}
            hasTranscript={!!lines?.length}
            hasAudio={tracks.length > 0}
            onDeleted={onBack}
            onDone={(msg) => {
              setError(null);
              setNotice(msg);
            }}
            onError={onError}
          />
        </header>
      )}

      {tracks.length > 0 && (
        <div className="sticky top-0 z-10 -mx-2 mt-4 bg-background px-2 py-2">
          <PlayerBar player={player} tracks={tracks} lines={lines ?? []} meName={meName} />
        </div>
      )}

      {error && (
        <p className="mt-4 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}
      {notice && !error && (
        <p className="mt-4 truncate text-sm text-muted-foreground" role="status" title={notice}>
          {notice}
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

      {lines && lines.length === 0 && !running && <p className="mt-8 text-sm text-muted-foreground">No transcript yet.</p>}

      {lines && lines.length > 0 && (
        <Tabs value={summariesOn ? tab : "transcript"} onValueChange={setTab} className="mt-4">
          {summariesOn && (
            <TabsList>
              <TabsTrigger value="transcript">Transcript</TabsTrigger>
              <TabsTrigger value="summary">Summary</TabsTrigger>
            </TabsList>
          )}
          <TabsContent value="transcript" className="pt-2">
            <div className="grid gap-6 lg:grid-cols-[1fr_15rem]">
              <Transcript
                lines={lines}
                speakers={speakers}
                meName={meName}
                player={tracks.length ? player : null}
                onChanged={loadTranscript}
                onError={onError}
              />
              <aside className="order-first lg:order-last">
                <div className="lg:sticky lg:top-36">
                  <SpeakersPanel speakers={speakers} meName={meName} onChanged={loadTranscript} onError={onError} />
                </div>
              </aside>
            </div>
          </TabsContent>
          {summariesOn && (
            <TabsContent value="summary" className="max-w-3xl pt-2">
              <SummaryPanel id={id} hasTranscript={!!lines.length} onOpenSettings={() => onOpenSettings("summary")} />
            </TabsContent>
          )}
        </Tabs>
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
