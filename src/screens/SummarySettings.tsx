import { useCallback, useEffect, useState } from "react";
import { ChevronDown, Pencil, Plus, RotateCcw, Trash2 } from "lucide-react";
import { cn } from "cn";
import { SettingsRow } from "@/components/SettingsRow";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { ipc } from "@/lib/ipc";
import { useModels, type ModelView } from "@/lib/models";
import { summaries, type SummaryMode, type Template } from "@/lib/summary";
import { ModelCard } from "@/screens/TranscriptionSettings";

export function SummarySettings() {
  const { info, progress, errors } = useModels();
  const [mode, setMode] = useState<SummaryMode>("auto");
  const [autoTitle, setAutoTitle] = useState(true);
  const [templates, setTemplates] = useState<Template[]>([]);
  const [defaultTemplate, setDefaultTemplate] = useState<number | null>(null);
  const [editing, setEditing] = useState<Template | "new" | null>(null);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadTemplates = useCallback(() => {
    summaries
      .templates()
      .then(async (all) => {
        setTemplates(all);
        const saved = Number(await ipc.getSetting("summary.template"));
        setDefaultTemplate(all.find((t) => t.id === saved)?.id ?? all[0]?.id ?? null);
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    ipc.getSetting("summary.mode").then((v) => v && setMode(v as SummaryMode));
    ipc.getSetting("summary.autoTitle").then((v) => setAutoTitle(v !== "false"));
    loadTemplates();
  }, [loadTemplates]);

  const save = (key: string, value: string) =>
    ipc
      .setSetting(key, value)
      .then(() => setError(null))
      .catch((e) => setError(String(e)));

  const all = info?.models.filter((m) => m.category === "summary") ?? [];
  const standard = all.filter((m) => !m.advanced);
  const advanced = all.filter((m) => m.advanced);
  const selected = all.find((m) => m.selected);
  const card = (m: ModelView) => (
    <ModelCard key={m.id} model={m} downloaded={progress[m.id] ?? m.downloadedBytes} error={errors[m.id]} onError={setError} />
  );

  return (
    <div>
      <SettingsRow
        label="Summaries"
        hint="Written on this computer by a local AI model. Automatic summaries follow transcription, whether it starts by itself or you start it."
      >
        <Select
          value={mode}
          onValueChange={(v) => {
            setMode(v as SummaryMode);
            save("summary.mode", v);
          }}
        >
          <SelectTrigger className="w-56" aria-label="Summaries">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="auto">Automatically</SelectItem>
            <SelectItem value="manual">When I ask</SelectItem>
            <SelectItem value="never">Off</SelectItem>
          </SelectContent>
        </Select>
      </SettingsRow>

      {mode !== "never" && (
        <>
          <SettingsRow label="Name untitled meetings" hint="Gives meetings without a title a short one that says what they were about.">
            <Switch
              checked={autoTitle}
              onCheckedChange={(v) => {
                setAutoTitle(v);
                save("summary.autoTitle", String(v));
              }}
              aria-label="Name untitled meetings"
            />
          </SettingsRow>
          <SettingsRow label="Template" hint="Used unless you pick another one for a meeting.">
            <Select
              value={defaultTemplate != null ? String(defaultTemplate) : undefined}
              onValueChange={(v) => {
                setDefaultTemplate(Number(v));
                save("summary.template", v);
              }}
            >
              <SelectTrigger className="w-56" aria-label="Default template">
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
          </SettingsRow>

          <h3 className="mt-6 text-sm font-medium">Summary model</h3>
          <p className="mt-1 text-xs text-muted-foreground">
            Larger models write better summaries but need more memory and take longer. Nothing is uploaded.
          </p>
          <div className="mt-3 grid gap-2">{standard.map(card)}</div>
          {advanced.length > 0 && (
            <div className="mt-3">
              <button
                type="button"
                onClick={() => setShowAdvanced((v) => !v)}
                aria-expanded={showAdvanced}
                className="flex items-center gap-1 text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:underline"
              >
                <ChevronDown className={cn("size-4 transition-transform", showAdvanced && "rotate-180")} aria-hidden />
                Advanced models ({advanced.length})
              </button>
              {showAdvanced && <div className="mt-2 grid gap-2">{advanced.map(card)}</div>}
            </div>
          )}
          <p className="mt-3 text-sm text-muted-foreground" role="status">
            {selected ? `Using ${selected.name} for summaries.` : "No summary model yet."}
          </p>

          <div className="mt-6 flex items-center">
            <h3 className="flex-1 text-sm font-medium">Templates</h3>
            <Button size="sm" variant="outline" onClick={() => setEditing("new")}>
              <Plus aria-hidden />
              New template
            </Button>
          </div>
          <ul className="mt-2 divide-y divide-border border-y border-border">
            {templates.map((t) => (
              <li key={t.id} className="flex items-center gap-2 py-2">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm">{t.name}</div>
                  <div className="truncate text-xs text-muted-foreground">
                    {t.sections.length ? t.sections.join(" · ") : "Sections chosen by the model"}
                  </div>
                </div>
                <Button size="icon-sm" variant="ghost" aria-label={`Edit ${t.name}`} onClick={() => setEditing(t)}>
                  <Pencil aria-hidden />
                </Button>
                {t.builtin ? (
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={`Reset ${t.name} to its original wording`}
                    title="Reset to original"
                    onClick={() => summaries.resetTemplate(t.id).then(loadTemplates).catch((e) => setError(String(e)))}
                  >
                    <RotateCcw aria-hidden />
                  </Button>
                ) : (
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    aria-label={`Delete ${t.name}`}
                    onClick={() => summaries.deleteTemplate(t.id).then(loadTemplates).catch((e) => setError(String(e)))}
                  >
                    <Trash2 aria-hidden />
                  </Button>
                )}
              </li>
            ))}
          </ul>
        </>
      )}

      {error && (
        <p className="mt-3 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}

      {editing && (
        <TemplateDialog
          template={editing === "new" ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            loadTemplates();
          }}
        />
      )}
    </div>
  );
}

function TemplateDialog({
  template,
  onClose,
  onSaved,
}: {
  template: Template | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState(template?.name ?? "");
  const [instructions, setInstructions] = useState(template?.instructions ?? "");
  const [sections, setSections] = useState((template?.sections ?? ["Summary", "Action items"]).join("\n"));
  const [error, setError] = useState<string | null>(null);

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{template ? "Edit template" : "New template"}</DialogTitle>
          <DialogDescription>
            Tell the model what matters in this kind of meeting. It only ever uses what was said.
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            summaries
              .saveTemplate({
                id: template?.id ?? null,
                name,
                instructions,
                sections: sections.split("\n"),
              })
              .then(onSaved)
              .catch((err) => setError(String(err)));
          }}
        >
          <div className="grid gap-1.5">
            <Label htmlFor="tpl-name">Name</Label>
            <Input id="tpl-name" value={name} maxLength={60} onChange={(e) => setName(e.target.value)} autoFocus />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="tpl-instructions">What to focus on</Label>
            <textarea
              id="tpl-instructions"
              value={instructions}
              maxLength={2000}
              rows={4}
              onChange={(e) => setInstructions(e.target.value)}
              className="w-full resize-y rounded-md border border-input bg-transparent px-2.5 py-2 text-sm outline-none select-text focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="tpl-sections">Sections, one per line</Label>
            <textarea
              id="tpl-sections"
              value={sections}
              rows={5}
              onChange={(e) => setSections(e.target.value)}
              className="w-full resize-y rounded-md border border-input bg-transparent px-2.5 py-2 text-sm outline-none select-text focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
            />
            <p className="text-xs text-muted-foreground">Leave empty to let the model choose its own headings.</p>
          </div>
          {error && (
            <p className="text-sm text-destructive" role="alert">
              {error}
            </p>
          )}
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit">Save</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
