import { useState } from "react";
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
import { meeting as api, type ExportFormat } from "@/lib/meeting";
import type { Meeting } from "@/lib/recorder";

const FORMATS: [ExportFormat, string][] = [
  ["md", "Markdown (.md)"],
  ["txt", "Plain text (.txt)"],
  ["srt", "Subtitles (.srt)"],
  ["vtt", "Web subtitles (.vtt)"],
  ["json", "JSON with word timings (.json)"],
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
          <DropdownMenuItem
            disabled={!hasAudio}
            onSelect={() =>
              api
                .exportAudio(m.id, m.startedAt)
                .then(saved)
                .catch((e) => onError(String(e)))
            }
          >
            Export audio…
          </DropdownMenuItem>
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
