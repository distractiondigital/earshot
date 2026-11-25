import { useState } from "react";
import { Pause, Play, RotateCcw, RotateCw, Volume2, VolumeX } from "lucide-react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { clock, speakerColor, type Player, type TrackAudio } from "@/lib/meeting";
import type { Utterance } from "@/lib/models";

const RATES = [0.75, 1, 1.25, 1.5, 1.75, 2];

/** Thin strip under the scrub bar showing who speaks when. */
function SpeakerStrip({ lines, durationMs }: { lines: Utterance[]; durationMs: number }) {
  if (!durationMs) return null;
  return (
    <div className="relative h-1 w-full overflow-hidden rounded-full bg-muted" aria-hidden>
      {lines.map((u) => (
        <div
          key={u.id}
          className={cn("absolute inset-y-0 opacity-70", speakerColor(u.speakerLabel, u.isMe).bg)}
          style={{
            left: `${(u.startMs / durationMs) * 100}%`,
            width: `${Math.max(0.15, ((u.endMs - u.startMs) / durationMs) * 100)}%`,
          }}
        />
      ))}
    </div>
  );
}

export function PlayerBar({
  player: p,
  tracks,
  lines,
  meName,
}: {
  player: Player;
  tracks: TrackAudio[];
  lines: Utterance[];
  meName: string;
}) {
  // While dragging, show the drag position instead of the playing time.
  const [drag, setDrag] = useState<number | null>(null);
  const shown = drag ?? p.timeMs;
  const trackName = (t: TrackAudio) => (t.kind === "mic" ? meName : "Meeting");

  return (
    <div className="rounded-lg border bg-card p-3">
      <div className="flex items-center gap-2">
        <Button
          size="icon-sm"
          variant="ghost"
          aria-label="Back 10 seconds"
          title="Back 10 seconds (←)"
          onClick={() => p.seek(p.timeMs - 10_000)}
        >
          <RotateCcw aria-hidden />
        </Button>
        <Button
          size="icon"
          className="rounded-full"
          aria-label={p.playing ? "Pause" : "Play"}
          title={p.playing ? "Pause (Space)" : "Play (Space)"}
          onClick={() => (p.playing ? p.pause() : p.play())}
        >
          {p.playing ? <Pause aria-hidden /> : <Play aria-hidden />}
        </Button>
        <Button
          size="icon-sm"
          variant="ghost"
          aria-label="Forward 10 seconds"
          title="Forward 10 seconds (→)"
          onClick={() => p.seek(p.timeMs + 10_000)}
        >
          <RotateCw aria-hidden />
        </Button>

        <div className="mx-2 flex min-w-0 flex-1 flex-col gap-1.5">
          <Slider
            min={0}
            max={Math.max(1, p.durationMs)}
            step={100}
            value={[shown]}
            onValueChange={([v]) => setDrag(v)}
            onValueCommit={([v]) => {
              p.seek(v);
              setDrag(null);
            }}
            aria-label="Playback position"
          />
          <SpeakerStrip lines={lines} durationMs={p.durationMs} />
        </div>

        <span className="w-24 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
          {clock(shown)} / {clock(p.durationMs)}
        </span>
      </div>

      <div className="mt-2 flex flex-wrap items-center gap-2">
        {tracks.map((t) => (
          <Button
            key={t.kind}
            size="xs"
            variant={p.muted[t.kind] ? "ghost" : "outline"}
            aria-pressed={!p.muted[t.kind]}
            title={p.muted[t.kind] ? `Unmute ${trackName(t)}` : `Mute ${trackName(t)}`}
            onClick={() => p.toggleMute(t.kind)}
            className={cn(p.muted[t.kind] && "text-muted-foreground line-through")}
          >
            {p.muted[t.kind] ? <VolumeX aria-hidden /> : <Volume2 aria-hidden />}
            {trackName(t)}
          </Button>
        ))}
        <div className="ml-auto flex items-center gap-2">
          <span className="text-xs text-muted-foreground">Speed</span>
          <Select value={String(p.rate)} onValueChange={(v) => p.setRate(Number(v))}>
            <SelectTrigger size="sm" className="w-20" aria-label="Playback speed">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {RATES.map((r) => (
                <SelectItem key={r} value={String(r)}>
                  {r}×
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>
      {p.error && (
        <p className="mt-2 text-xs text-destructive" role="alert">
          {p.error}
        </p>
      )}
    </div>
  );
}
