import { useEffect, useState, type ReactNode } from "react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { meeting, speakerColor, speakerDisplayName, type Speaker } from "@/lib/meeting";

/** Rename a speaker or merge it into another one. */
export function SpeakerPopover({
  speaker,
  speakers,
  meName,
  onChanged,
  onError,
  align = "start",
  children,
}: {
  speaker: Speaker;
  speakers: Speaker[];
  meName: string;
  onChanged: () => void;
  onError: (e: string) => void;
  align?: "start" | "end";
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState(speaker.name ?? "");
  useEffect(() => {
    if (open) setName(speaker.name ?? "");
  }, [open, speaker.name]);

  const run = (p: Promise<unknown>) =>
    p
      .then(() => {
        setOpen(false);
        onChanged();
      })
      .catch((e) => onError(String(e)));

  const others = speakers.filter((s) => s.id !== speaker.id);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>{children}</PopoverTrigger>
      <PopoverContent align={align} className="w-72">
        {speaker.isMe ? (
          <p className="text-sm text-muted-foreground">
            This is your microphone. Change the name shown for it in Settings → Speakers.
          </p>
        ) : (
          <>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                run(meeting.renameSpeaker(speaker.id, name));
              }}
            >
              <label className="text-xs font-medium" htmlFor={`speaker-name-${speaker.id}`}>
                Name for {speaker.label} in this meeting
              </label>
              <div className="mt-1.5 flex gap-2">
                <Input
                  id={`speaker-name-${speaker.id}`}
                  value={name}
                  maxLength={60}
                  placeholder={speaker.label}
                  autoFocus
                  onChange={(e) => setName(e.target.value)}
                />
                <Button type="submit" size="sm">
                  Save
                </Button>
              </div>
            </form>
            {others.length > 0 && (
              <div className="mt-4">
                <p className="text-xs font-medium">Same person as…</p>
                <p className="text-xs text-muted-foreground">
                  Moves all of {speakerDisplayName(speaker, meName)}'s lines to that speaker.
                </p>
                <div className="mt-1.5 flex flex-wrap gap-1.5">
                  {others.map((o) => (
                    <Button
                      key={o.id}
                      size="xs"
                      variant="outline"
                      onClick={() => run(meeting.mergeSpeakers(speaker.id, o.id))}
                    >
                      <span className={cn("size-2 rounded-full", speakerColor(o.label, o.isMe).bg)} aria-hidden />
                      {speakerDisplayName(o, meName)}
                    </Button>
                  ))}
                </div>
              </div>
            )}
          </>
        )}
      </PopoverContent>
    </Popover>
  );
}
