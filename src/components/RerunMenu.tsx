import { useEffect, useState } from "react";
import { ChevronDown, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Slider } from "@/components/ui/slider";
import { ipc } from "@/lib/ipc";
import { meeting } from "@/lib/meeting";
import { models, processing, type ModelView } from "@/lib/models";

const DEFAULT_SENSITIVITY = 50;

/** "Transcribe again with…" and "Separate speakers again…" for one meeting. */
export function RerunMenu({
  id,
  hasTranscript,
  hasEdits,
  speakerHint,
  onQueued,
  onError,
}: {
  id: number;
  hasTranscript: boolean;
  hasEdits: boolean;
  speakerHint: number | null;
  onQueued: () => void;
  onError: (e: string) => void;
}) {
  const [dialog, setDialog] = useState<"transcribe" | "speakers" | null>(null);

  if (!hasTranscript) {
    return (
      <Button variant="outline" size="sm" onClick={() => processing.transcribe(id).then(onQueued).catch((e) => onError(String(e)))}>
        <RotateCw aria-hidden />
        Transcribe
      </Button>
    );
  }

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm">
            <RotateCw aria-hidden />
            Run again
            <ChevronDown aria-hidden />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onSelect={() => setDialog("transcribe")}>Transcribe again…</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => setDialog("speakers")}>Separate speakers again…</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {dialog === "transcribe" && (
        <TranscribeDialog id={id} hasEdits={hasEdits} onClose={() => setDialog(null)} onQueued={onQueued} onError={onError} />
      )}
      {dialog === "speakers" && (
        <SpeakersDialog
          id={id}
          hasEdits={hasEdits}
          speakerHint={speakerHint}
          onClose={() => setDialog(null)}
          onQueued={onQueued}
          onError={onError}
        />
      )}
    </>
  );
}

function EditsNote({ hasEdits }: { hasEdits: boolean }) {
  return (
    <p className="text-xs text-muted-foreground">
      Names you gave speakers are kept where the new speakers clearly match the old ones.
      {hasEdits && " Lines you edited by hand go back to what the transcription heard."}
    </p>
  );
}

function TranscribeDialog({
  id,
  hasEdits,
  onClose,
  onQueued,
  onError,
}: {
  id: number;
  hasEdits: boolean;
  onClose: () => void;
  onQueued: () => void;
  onError: (e: string) => void;
}) {
  const [ready, setReady] = useState<ModelView[]>([]);
  const [model, setModel] = useState<string>("");
  useEffect(() => {
    models.list().then((info) => {
      const r = info.models.filter((m) => m.category === "transcription" && m.state === "ready");
      setReady(r);
      setModel((r.find((m) => m.selected) ?? r[0])?.id ?? "");
    });
  }, []);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Transcribe again</DialogTitle>
          <DialogDescription>Replaces this meeting's transcript. Speakers are separated again afterwards.</DialogDescription>
        </DialogHeader>
        {ready.length === 0 ? (
          <p className="text-sm text-muted-foreground">Download a transcription model in Settings → Transcription first.</p>
        ) : (
          <label className="grid gap-1.5 text-sm">
            Model
            <Select value={model} onValueChange={setModel}>
              <SelectTrigger className="w-full" aria-label="Transcription model">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {ready.map((m) => (
                  <SelectItem key={m.id} value={m.id}>
                    {m.name}
                    {m.selected && " (default)"}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </label>
        )}
        <EditsNote hasEdits={hasEdits} />
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={!model}
            onClick={() =>
              processing
                .transcribe(id, false, model)
                .then(() => {
                  onClose();
                  onQueued();
                })
                .catch((e) => onError(String(e)))
            }
          >
            Transcribe
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function SpeakersDialog({
  id,
  hasEdits,
  speakerHint,
  onClose,
  onQueued,
  onError,
}: {
  id: number;
  hasEdits: boolean;
  speakerHint: number | null;
  onClose: () => void;
  onQueued: () => void;
  onError: (e: string) => void;
}) {
  const [count, setCount] = useState(speakerHint ? String(speakerHint) : "auto");
  const [sensitivity, setSensitivity] = useState(DEFAULT_SENSITIVITY);
  useEffect(() => {
    ipc.getSetting("speakers.sensitivity").then((v) => {
      const n = Number(v);
      if (v && Number.isFinite(n)) setSensitivity(n);
    });
  }, []);
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Separate speakers again</DialogTitle>
          <DialogDescription>
            Keeps the transcript and works out who said what again. Takes about a minute per hour of audio.
          </DialogDescription>
        </DialogHeader>
        <label className="grid gap-1.5 text-sm">
          People in the meeting (not counting you)
          <Select value={count} onValueChange={setCount}>
            <SelectTrigger className="w-40" aria-label="Number of remote speakers">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">Detect automatically</SelectItem>
              {Array.from({ length: 10 }, (_, i) => i + 1).map((n) => (
                <SelectItem key={n} value={String(n)}>
                  {n}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </label>
        {count === "auto" && (
          <div className="grid gap-1.5 text-sm">
            Sensitivity
            <Slider
              min={0}
              max={100}
              step={5}
              value={[sensitivity]}
              onValueChange={([v]) => setSensitivity(v)}
              aria-label="Speaker sensitivity"
            />
            <div className="flex justify-between text-xs whitespace-nowrap text-muted-foreground">
              <span>Fewer speakers</span>
              <span>More speakers</span>
            </div>
          </div>
        )}
        <EditsNote hasEdits={hasEdits} />
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            onClick={() =>
              meeting
                .separateSpeakers(id, count === "auto" ? null : Number(count), count === "auto" ? sensitivity : null)
                .then(() => {
                  onClose();
                  onQueued();
                })
                .catch((e) => onError(String(e)))
            }
          >
            Separate speakers
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
