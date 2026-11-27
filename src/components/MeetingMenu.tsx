import { useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { MoreHorizontal } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { AUDIO_MIX_STARTED, meeting as api, type AudioExport, type ExportFormat } from "@/lib/meeting";
import type { Meeting } from "@/lib/recorder";

const FORMATS: [ExportFormat, string][] = [
  ["md", "Markdown (.md)"],
  ["txt", "Plain text (.txt)"],
  ["srt", "Subtitles (.srt)"],
  ["vtt", "Web subtitles (.vtt)"],
  ["json", "JSON with word timings (.json)"],
];

const AUDIO: [AudioExport, string, string][] = [
  ["tracks", "Separate tracks (FLAC)", "Lossless, one file per track"],
  ["flac", "Mixed (FLAC)", "Lossless, one file"],
  ["mp3", "Mixed (MP3)", "Smaller, plays anywhere"],
];

/** Export and delete for one meeting. */
export function MeetingMenu({
  meeting: m,
  hasTranscript,
  hasAudio,
  onDeleted,
  onDone,
  onError,
}: {
  meeting: Meeting;
  hasTranscript: boolean;
  hasAudio: boolean;
  onDeleted: () => void;
  onDone: (message: string) => void;
  onError: (e: string) => void;
}) {
  const [confirm, setConfirm] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const saved = (p: string | null) => p && onDone(`Saved to ${p}`);

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="icon-sm" aria-label="More actions">
            <MoreHorizontal aria-hidden />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuSub>
            <DropdownMenuSubTrigger disabled={!hasTranscript}>Export transcript</DropdownMenuSubTrigger>
            <DropdownMenuSubContent>
              {FORMATS.map(([f, label]) => (
                <DropdownMenuItem
                  key={f}
                  onSelect={() =>
                    api
                      .exportTranscript(m.id, f, m.startedAt)
                      .then(saved)
                      .catch((e) => onError(String(e)))
                  }
                >
                  {label}
                </DropdownMenuItem>
              ))}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger disabled={!hasAudio}>Export audio</DropdownMenuSubTrigger>
            <DropdownMenuSubContent>
              {AUDIO.map(([kind, label, hint]) => (
                <DropdownMenuItem
                  key={kind}
                  className="flex-col items-start gap-0"
                  onSelect={async () => {
                    // A mix takes a moment for a long meeting: say so once the file is chosen.
                    const unlisten = await listen<number>(AUDIO_MIX_STARTED, (e) => {
                      if (e.payload === m.id) onDone("Mixing audio…");
                    });
                    api
                      .exportAudio(m.id, kind, m.startedAt)
                      .then(saved)
                      .catch((e) => onError(String(e)))
                      .finally(unlisten);
                  }}
                >
                  <span>{label}</span>
                  <span className="text-xs text-muted-foreground">{hint}</span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={() => setConfirm(true)}>
            Delete meeting…
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <Dialog open={confirm} onOpenChange={(o) => !deleting && setConfirm(o)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete this meeting?</DialogTitle>
            <DialogDescription>
              “{m.title || "Untitled meeting"}” will be removed permanently: its audio recordings, transcript and
              speaker names. This can't be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConfirm(false)} disabled={deleting}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={deleting}
              onClick={() => {
                setDeleting(true);
                api
                  .remove(m.id)
                  .then(() => {
                    setConfirm(false);
                    onDeleted();
                  })
                  .catch((e) => onError(String(e)))
                  .finally(() => setDeleting(false));
              }}
            >
              {deleting ? "Deleting…" : "Delete permanently"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
