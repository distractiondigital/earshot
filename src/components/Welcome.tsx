import { useEffect, useState } from "react";
import { Check } from "lucide-react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { formatBytes, models, useModels, type ModelView } from "@/lib/models";
import { recorder } from "@/lib/recorder";

/**
 * First-run guide, shown on the Meetings screen until the first meeting exists: get the speech
 * model (required), optionally a summary model, then record. Uses the same downloads as Settings.
 */
export function Welcome({ onOpenSettings }: { onOpenSettings: (tab: string) => void }) {
  const { info, progress, errors } = useModels();
  const [hotkey, setHotkey] = useState("");
  useEffect(() => {
    recorder.getHotkey().then(setHotkey).catch(() => {});
  }, []);
  if (!info) return null;

  const speech = info.models.filter((m) => m.category === "transcription");
  const speechModel = speech.find((m) => m.selected) ?? speech.find((m) => m.id === info.recommended);
  const summary = info.models.filter((m) => m.category === "summary");
  const summaryModel = summary.find((m) => m.selected) ?? summary.find((m) => m.id === info.recommendedSummary);
  const mac = navigator.userAgent.includes("Mac");
  const shortcut = hotkey.replace("CommandOrControl", mac ? "Cmd" : "Ctrl").replace(/\+/g, " + ");

  return (
    <section aria-labelledby="welcome-title" className="w-full max-w-lg">
      <h2 id="welcome-title" className="text-base font-semibold">
        Welcome to Earshot
      </h2>
      <p className="mt-1 text-sm text-muted-foreground">
        Earshot records your calls and turns them into searchable transcripts and summaries, entirely on
        this computer. Nothing is uploaded.
      </p>
      <ol className="mt-5 space-y-4">
        <Step
          n={1}
          done={speechModel?.state === "ready"}
          title="Get the speech model"
          body={
            speechModel
              ? `${speechModel.name} (${formatBytes(speechModel.sizeBytes)}) turns speech into text. It's the best fit for this computer; speaker models download with it.`
              : "Choose a speech model."
          }
        >
          {speechModel && (
            <ModelAction model={speechModel} downloaded={progress[speechModel.id]} error={errors[speechModel.id]} />
          )}
          <Button variant="link" size="sm" className="h-auto p-0 text-xs" onClick={() => onOpenSettings("transcription")}>
            Other models
          </Button>
        </Step>
        <Step
          n={2}
          done={summaryModel?.state === "ready"}
          title="Add summaries (optional)"
          body={
            summaryModel
              ? `${summaryModel.name} (${formatBytes(summaryModel.sizeBytes)}) writes a summary, decisions and action items after each meeting.`
              : "Pick a summary model in Settings."
          }
        >
          {summaryModel && (
            <ModelAction model={summaryModel} downloaded={progress[summaryModel.id]} error={errors[summaryModel.id]} />
          )}
          <Button variant="link" size="sm" className="h-auto p-0 text-xs" onClick={() => onOpenSettings("summary")}>
            Other models
          </Button>
        </Step>
        <Step
          n={3}
          done={false}
          title="Record a meeting"
          body={`Press Record below${shortcut ? `, or ${shortcut} from any app` : ""}. Your mic and the meeting audio are kept as separate tracks. Let people know you're recording.`}
        />
      </ol>
    </section>
  );
}

function Step({
  n,
  done,
  title,
  body,
  children,
}: {
  n: number;
  done: boolean;
  title: string;
  body: string;
  children?: React.ReactNode;
}) {
  return (
    <li className="flex gap-3">
      <span
        className={cn(
          "mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border text-xs tabular-nums",
          done ? "border-primary bg-primary text-primary-foreground" : "border-border text-muted-foreground",
        )}
        aria-hidden
      >
        {done ? <Check className="size-3" /> : n}
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-sm font-medium">
          {title}
          {done && <span className="sr-only"> (done)</span>}
        </p>
        <p className="text-sm text-muted-foreground">{body}</p>
        {children && <div className="mt-2 flex items-center gap-3">{children}</div>}
      </div>
    </li>
  );
}

function ModelAction({ model, downloaded, error }: { model: ModelView; downloaded?: number; error?: string }) {
  const [busy, setBusy] = useState(false);
  if (model.state === "ready") {
    return (
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Check className="size-3.5" aria-hidden /> Ready
      </span>
    );
  }
  if (model.state === "downloading") {
    const pct = Math.min(100, Math.round(((downloaded ?? model.downloadedBytes) / model.sizeBytes) * 100));
    return (
      <div className="flex w-56 items-center gap-2">
        <Progress value={pct} aria-label={`Downloading ${model.name}`} className="flex-1" />
        <span className="w-9 text-right text-xs text-muted-foreground tabular-nums">{pct}%</span>
        <Button size="sm" variant="ghost" className="h-7 px-2" onClick={() => models.cancelDownload(model.id)}>
          Cancel
        </Button>
      </div>
    );
  }
  return (
    <>
      <Button
        size="sm"
        variant="outline"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          models.download(model.id).finally(() => setBusy(false));
        }}
      >
        Download
      </Button>
      {error && <span className="text-xs text-destructive">{error}</span>}
    </>
  );
}
