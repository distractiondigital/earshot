import { useEffect, useState } from "react";
import { Pause, Play, Square } from "lucide-react";
import { cn } from "cn";
import { ipc } from "@/lib/ipc";
import { formatElapsed, recorder, useLevels, useRecorderState } from "@/lib/recorder";

/** Floating recorder: red dot, elapsed time, Mic/Meeting meters, pause, stop. Nothing else. */
export function Pill() {
  const state = useRecorderState();
  const levels = useLevels();
  const [reminder, setReminder] = useState(false);
  const paused = state.status === "paused";
  const elapsed = levels?.elapsedMs ?? state.elapsedMs;

  useEffect(() => {
    document.documentElement.classList.add("pill");
    ipc.getSetting("recording.consentReminder").then((v) => {
      if (v === "true") {
        setReminder(true);
        setTimeout(() => setReminder(false), 6000);
      }
    });
  }, []);

  return (
    <div
      data-tauri-drag-region
      className="flex h-11 w-full items-center gap-2.5 rounded-full border border-border bg-background/95 pr-1.5 pl-3.5 text-foreground shadow-sm select-none"
    >
      <span
        aria-hidden
        className={cn("size-2.5 shrink-0 rounded-full bg-red-500", !paused && "motion-safe:animate-pulse")}
      />
      {reminder ? (
        <span data-tauri-drag-region className="flex-1 truncate text-xs text-muted-foreground">
          Let everyone know you're recording.
        </span>
      ) : (
        <>
          <span data-tauri-drag-region className="w-14 text-sm tabular-nums" aria-label="Elapsed time">
            {formatElapsed(elapsed)}
          </span>
          <div data-tauri-drag-region className="flex flex-1 flex-col gap-1">
            <Meter label="Mic" value={levels?.mic} failed={levels?.micFailed} />
            <Meter label="Meeting" value={levels?.system} failed={levels?.systemFailed} />
          </div>
        </>
      )}
      <button
        type="button"
        onClick={() => recorder.pause(!paused)}
        aria-label={paused ? "Resume recording" : "Pause recording"}
        className="grid size-8 place-items-center rounded-full text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
      >
        {paused ? <Play className="size-4" /> : <Pause className="size-4" />}
      </button>
      <button
        type="button"
        onClick={() => recorder.stop()}
        aria-label="Stop recording"
        className="grid size-8 place-items-center rounded-full bg-red-500 text-white outline-none hover:bg-red-600 focus-visible:ring-2 focus-visible:ring-ring"
      >
        <Square className="size-3.5 fill-current" />
      </button>
    </div>
  );
}

function Meter({ label, value, failed }: { label: string; value: number | null | undefined; failed?: boolean }) {
  if (value === undefined || value === null) {
    return <div className="h-1 rounded-full bg-muted/50" aria-label={`${label}: not recorded`} />;
  }
  return (
    <div
      data-tauri-drag-region
      className="h-1 overflow-hidden rounded-full bg-muted"
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={value}
      title={failed ? `${label}: device stopped` : label}
    >
      <div
        className={cn("h-full rounded-full transition-[width] duration-75", failed ? "bg-red-500" : "bg-foreground/70")}
        style={{ width: `${Math.round(value * 100)}%` }}
      />
    </div>
  );
}
