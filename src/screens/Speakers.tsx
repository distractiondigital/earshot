import { useCallback, useEffect, useState } from "react";
import { AudioLines, ChevronDown, Pencil, Trash2 } from "lucide-react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { useModels } from "@/lib/models";
import { profiles, type Profile, type ProfileMeeting } from "@/lib/meeting";

/** Saved voices: recognised automatically in new meetings. */
export function Speakers({ onOpenMeeting }: { onOpenMeeting: (id: number) => void }) {
  const [list, setList] = useState<Profile[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { info } = useModels();
  const voiceModel = info?.models.find((m) => m.category === "embedding" && m.selected);

  const load = useCallback(() => {
    profiles.list().then(setList).catch((e) => setError(String(e)));
  }, []);
  useEffect(load, [load]);

  return (
    <div className="mx-auto max-w-3xl p-8">
      <h1 className="text-lg font-semibold">Speakers</h1>
      <p className="mt-1 text-sm text-muted-foreground">
        People whose voices you've saved. Earshot suggests their names when it hears them in new meetings.
        Voices are stored on this computer only, as numbers that describe the voice, not as recordings.
      </p>
      {error && (
        <p className="mt-4 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}
      {list && list.length === 0 && (
        <div className="mt-8 rounded-lg border border-dashed p-6 text-sm text-muted-foreground">
          <AudioLines className="mb-2 size-5" aria-hidden />
          No saved voices yet. Open a meeting, click a speaker's name and choose <em>Save voice</em>.
        </div>
      )}
      {list && list.length > 0 && (
        <ul className="mt-6 divide-y divide-border border-y border-border">
          {list.map((p) => (
            <ProfileRow
              key={p.id}
              profile={p}
              currentModel={voiceModel?.id ?? null}
              currentModelName={voiceModel?.name ?? null}
              onChanged={load}
              onOpenMeeting={onOpenMeeting}
              onError={setError}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function ProfileRow({
  profile: p,
  currentModel,
  currentModelName,
  onChanged,
  onOpenMeeting,
  onError,
}: {
  profile: Profile;
  currentModel: string | null;
  currentModelName: string | null;
  onChanged: () => void;
  onOpenMeeting: (id: number) => void;
  onError: (e: string) => void;
}) {
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(p.name);
  const [open, setOpen] = useState(false);
  const [meetings, setMeetings] = useState<ProfileMeeting[] | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const samples = p.samples.reduce((n, [, c]) => n + c, 0);
  const forCurrent = currentModel ? (p.samples.find(([m]) => m === currentModel)?.[1] ?? 0) : samples;

  useEffect(() => {
    if (open) profiles.meetings(p.id).then(setMeetings).catch((e) => onError(String(e)));
  }, [open, p.id, onError]);

  return (
    <li className="py-3">
      <div className="flex items-center gap-3">
        <AudioLines className="size-4 shrink-0 text-muted-foreground" aria-hidden />
        <div className="min-w-0 flex-1">
          {renaming ? (
            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                profiles
                  .rename(p.id, name)
                  .then(() => {
                    setRenaming(false);
                    onChanged();
                  })
                  .catch((err) => onError(String(err)));
              }}
            >
              <Input value={name} maxLength={60} autoFocus aria-label="Name" onChange={(e) => setName(e.target.value)} />
              <Button type="submit" size="sm">
                Save
              </Button>
              <Button type="button" size="sm" variant="ghost" onClick={() => setRenaming(false)}>
                Cancel
              </Button>
            </form>
          ) : (
            <>
              <div className="truncate text-sm font-medium">{p.name}</div>
              <div className="text-xs text-muted-foreground">
                {p.meetings === 1 ? "1 meeting" : `${p.meetings} meetings`}
                {p.lastSeen && ` · last heard ${new Date(p.lastSeen).toLocaleDateString(undefined, { dateStyle: "medium" })}`}
                {` · ${samples} voice ${samples === 1 ? "sample" : "samples"}`}
              </div>
              {currentModel && forCurrent === 0 && (
                <div className="mt-0.5 text-xs text-amber-700 dark:text-amber-300">
                  Saved with a different voice model. Confirm them in a meeting processed with {currentModelName} to keep
                  recognising them.
                </div>
              )}
            </>
          )}
        </div>
        {!renaming && (
          <>
            <Button size="icon-sm" variant="ghost" aria-label={`Rename ${p.name}`} onClick={() => setRenaming(true)}>
              <Pencil aria-hidden />
            </Button>
            <Button size="icon-sm" variant="ghost" aria-label={`Delete ${p.name}`} onClick={() => setConfirmDelete(true)}>
              <Trash2 aria-hidden />
            </Button>
            <Button
              size="icon-sm"
              variant="ghost"
              aria-label={open ? "Hide meetings" : "Show meetings"}
              aria-expanded={open}
              onClick={() => setOpen((o) => !o)}
            >
              <ChevronDown className={cn("transition-transform", open && "rotate-180")} aria-hidden />
            </Button>
          </>
        )}
      </div>
      {open && meetings && (
        <ul className="mt-2 ml-7 space-y-0.5">
          {meetings.length === 0 && <li className="text-xs text-muted-foreground">Not in any meeting now.</li>}
          {meetings.map((m) => (
            <li key={m.meetingId}>
              <button
                type="button"
                onClick={() => onOpenMeeting(m.meetingId)}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-sm outline-none hover:bg-muted focus-visible:bg-muted"
              >
                <span className="min-w-0 flex-1 truncate">{m.title || "Untitled meeting"}</span>
                {m.autoMatched && <span className="text-xs text-muted-foreground">suggested</span>}
                <span className="text-xs text-muted-foreground">
                  {new Date(m.startedAt).toLocaleDateString(undefined, { dateStyle: "medium" })}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <Dialog open={confirmDelete} onOpenChange={setConfirmDelete}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete {p.name}'s voice?</DialogTitle>
            <DialogDescription>
              Earshot will stop recognising {p.name}. Meetings keep their transcripts and any names you typed. This
              can't be undone.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setConfirmDelete(false)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={() =>
                profiles
                  .remove(p.id)
                  .then(() => {
                    setConfirmDelete(false);
                    onChanged();
                  })
                  .catch((e) => onError(String(e)))
              }
            >
              Delete voice
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </li>
  );
}
