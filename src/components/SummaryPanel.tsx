import { useEffect, useState } from "react";
import { Check, ChevronDown, Copy, Pencil, RotateCw, Sparkles, Square } from "lucide-react";
import { Markdown } from "@/components/Markdown";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Progress } from "@/components/ui/progress";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ipc } from "@/lib/ipc";
import { useModels } from "@/lib/models";
import { STAGE_LABEL, summaries, useSummary, type Template } from "@/lib/summary";

/** A meeting's summary: write it, watch it being written, read, edit, copy, regenerate. */
export function SummaryPanel({
  id,
  hasTranscript,
  onOpenSettings,
}: {
  id: number;
  hasTranscript: boolean;
  onOpenSettings: () => void;
}) {
  const { state, live, stage, error: loadError, refresh } = useSummary(id);
  const { info } = useModels();
  const [templates, setTemplates] = useState<Template[]>([]);
  const [templateId, setTemplateId] = useState<number | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    summaries
      .templates()
      .then(async (all) => {
        setTemplates(all);
        const saved = Number(await ipc.getSetting("summary.template"));
        setTemplateId(all.find((t) => t.id === saved)?.id ?? all[0]?.id ?? null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const ready = info?.models.filter((m) => m.category === "summary" && m.state === "ready") ?? [];
  const selected = ready.find((m) => m.selected);
  const job = state?.job;
  const running = job?.status === "running" || job?.status === "queued";
  const summary = state?.summary ?? null;
  const modelName = (mid: string) => info?.models.find((m) => m.id === mid)?.name ?? mid;

  const run = (tid: number | null, model: string | null = null) => {
    setError(null);
    summaries
      .summarize(id, tid, model)
      .then(refresh)
      .catch((e) => setError(String(e)));
  };

  if (!state) return loadError ? <p className="text-sm text-destructive">{loadError}</p> : null;

  const problem = error ?? (job?.status === "failed" ? job.error : null);
  const notice = job?.status === "done" && job.error ? job.error : null;

  // Being written.
  if (running) {
    const writing = stage?.stage === "summarize" && live.trim();
    return (
      <section aria-label="Summary" aria-busy="true">
        <div className="flex items-center gap-3">
          <Sparkles className="size-4 animate-pulse text-muted-foreground" aria-hidden />
          <span className="flex-1 text-sm text-muted-foreground" role="status">
            {job?.status === "queued" ? "Waiting for the transcription to finish…" : STAGE_LABEL[stage?.stage ?? "load_model"]}
          </span>
          <Button size="sm" variant="outline" onClick={() => summaries.cancel(id).catch((e) => setError(String(e)))}>
            <Square aria-hidden />
            Stop
          </Button>
        </div>
        {stage?.stage === "notes" && (
          <Progress value={stage.pct} className="mt-3" aria-label="Reading progress" />
        )}
        {writing && <Markdown text={live} className="mt-4" />}
      </section>
    );
  }

  // Nothing to summarize with.
  if (!selected && !summary) {
    return (
      <div className="rounded-lg border border-dashed p-6 text-sm text-muted-foreground">
        <Sparkles className="mb-2 size-5" aria-hidden />
        <p>
          Summaries are written on this computer by a local AI model. Nothing is uploaded. Download a summary model to
          get started.
        </p>
        <Button size="sm" variant="outline" className="mt-3" onClick={onOpenSettings}>
          Open Summary settings
        </Button>
      </div>
    );
  }

  if (!summary) {
    return (
      <div>
        {problem && (
          <p className="mb-3 text-sm text-destructive" role="alert">
            {problem}
          </p>
        )}
        {hasTranscript ? (
          <div className="flex flex-wrap items-center gap-2">
            <Select value={templateId != null ? String(templateId) : undefined} onValueChange={(v) => setTemplateId(Number(v))}>
              <SelectTrigger size="sm" className="w-48" aria-label="Template">
                <SelectValue placeholder="Template" />
              </SelectTrigger>
              <SelectContent>
                {templates.map((t) => (
                  <SelectItem key={t.id} value={String(t.id)}>
                    {t.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button size="sm" onClick={() => run(templateId)}>
              <Sparkles aria-hidden />
              {problem ? "Try again" : "Summarize"}
            </Button>
            {selected && <span className="text-xs text-muted-foreground">with {selected.name}</span>}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">The summary can be written once there's a transcript.</p>
        )}
      </div>
    );
  }

  // Editing.
  if (editing != null) {
    return (
      <section aria-label="Edit summary">
        <textarea
          value={editing}
          onChange={(e) => setEditing(e.target.value)}
          autoFocus
          aria-label="Summary (Markdown)"
          className="min-h-96 w-full resize-y rounded-md border border-input bg-transparent p-3 font-mono text-sm outline-none select-text focus-visible:ring-2 focus-visible:ring-ring"
        />
        <div className="mt-2 flex justify-end gap-2">
          <Button size="sm" variant="ghost" onClick={() => setEditing(null)}>
            Cancel
          </Button>
          <Button
            size="sm"
            onClick={() =>
              summaries
                .edit(id, editing)
                .then(() => {
                  setEditing(null);
                  refresh();
                })
                .catch((e) => setError(String(e)))
            }
          >
            Save
          </Button>
        </div>
      </section>
    );
  }

  return (
    <section aria-label="Summary">
      <div className="flex flex-wrap items-center gap-2">
        <p className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
          {summary.templateName || "Summary"} · {modelName(summary.modelId)} ·{" "}
          {new Date(summary.createdAt).toLocaleDateString(undefined, { dateStyle: "medium" })}
          {summary.edited && " · edited"}
        </p>
        <Button
          size="sm"
          variant="ghost"
          onClick={() =>
            navigator.clipboard.writeText(summary.markdown).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            })
          }
        >
          {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
          {copied ? "Copied" : "Copy"}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setEditing(summary.markdown)}>
          <Pencil aria-hidden />
          Edit
        </Button>
        {(selected || ready.length > 0) && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button size="sm" variant="outline">
                <RotateCw aria-hidden />
                Regenerate
                <ChevronDown aria-hidden />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-56">
              <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
                {summary.edited ? "Replaces your edits. Template:" : "Template:"}
              </DropdownMenuLabel>
              {templates.map((t) => (
                <DropdownMenuItem key={t.id} onSelect={() => run(t.id)}>
                  {t.name}
                  {t.id === summary.templateId && <Check className="ml-auto" aria-label="(current)" />}
                </DropdownMenuItem>
              ))}
              {ready.length > 1 && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuSub>
                    <DropdownMenuSubTrigger>With another model</DropdownMenuSubTrigger>
                    <DropdownMenuSubContent>
                      {ready.map((m) => (
                        <DropdownMenuItem key={m.id} onSelect={() => run(summary.templateId, m.id)}>
                          {m.name}
                        </DropdownMenuItem>
                      ))}
                    </DropdownMenuSubContent>
                  </DropdownMenuSub>
                </>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {problem && (
        <p className="mt-3 text-sm text-destructive" role="alert">
          {problem}
        </p>
      )}
      {notice && !problem && <p className="mt-3 text-sm text-muted-foreground">{notice}</p>}
      <Markdown text={summary.markdown} className="mt-3" />
    </section>
  );
}
