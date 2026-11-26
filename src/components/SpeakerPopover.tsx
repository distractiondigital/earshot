import { useEffect, useState, type ReactNode } from "react";
import { AudioLines, Check, X } from "lucide-react";
import { cn } from "cn";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { meeting, profiles, speakerColor, speakerDisplayName, type Profile, type Speaker } from "@/lib/meeting";

/** Rename a speaker, merge it into another one, and save or confirm its voice profile. */
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
  const [saved, setSaved] = useState<Profile[]>([]);
  useEffect(() => {
    if (!open) return;
    setName(speaker.name ?? "");
    if (!speaker.isMe) profiles.list().then(setSaved).catch(() => {});
  }, [open, speaker.name, speaker.isMe]);

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
      <PopoverContent align={align} className="w-80">
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
                  placeholder={speaker.profileName ?? speaker.label}
                  autoFocus
                  onChange={(e) => setName(e.target.value)}
                />
                <Button type="submit" size="sm">
                  Save
                </Button>
              </div>
            </form>

            <VoiceSection
              speaker={speaker}
              name={name.trim() || speaker.name || ""}
              saved={saved}
              run={run}
            />

            {others.length > 0 && (
              <div className="mt-4 border-t pt-3">
                <p className="text-xs font-medium">Same person as…</p>
                <p className="text-xs text-muted-foreground">
                  Moves all of {speakerDisplayName(speaker, meName)}'s lines to that speaker.
                </p>
                <div className="mt-1.5 flex flex-wrap gap-1.5">
                  {others.map((o) => (
                    <Button key={o.id} size="xs" variant="outline" onClick={() => run(meeting.mergeSpeakers(speaker.id, o.id))}>
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

/** Voice profile for one meeting speaker: confirm a suggestion, save the voice, or say who it is. */
function VoiceSection({
  speaker: s,
  name,
  saved,
  run,
}: {
  speaker: Speaker;
  name: string;
  saved: Profile[];
  run: (p: Promise<unknown>) => void;
}) {
  if (s.profileId && s.autoMatched) {
    return (
      <div className="mt-4 rounded-md bg-muted/60 p-2.5">
        <p className="text-xs font-medium">Sounds like {s.profileName}</p>
        <p className="text-xs text-muted-foreground">Recognised from a saved voice. Confirming helps future matches.</p>
        <div className="mt-2 flex gap-1.5">
          <Button size="xs" onClick={() => run(profiles.link(s.id, s.profileId!))}>
            <Check aria-hidden />
            Yes, it's {s.profileName}
          </Button>
          <Button size="xs" variant="ghost" onClick={() => run(profiles.unlink(s.id))}>
            <X aria-hidden />
            No
          </Button>
        </div>
      </div>
    );
  }
  if (s.profileId) {
    return (
      <div className="mt-4 flex items-center gap-2 text-xs">
        <AudioLines className="size-3.5 text-muted-foreground" aria-hidden />
        <span className="flex-1">
          Voice profile: <span className="font-medium">{s.profileName}</span>
        </span>
        <Button size="xs" variant="ghost" onClick={() => run(profiles.unlink(s.id))}>
          Not them
        </Button>
      </div>
    );
  }
  if (!s.hasVoice) {
    return (
      <p className="mt-4 text-xs text-muted-foreground">
        There isn't enough clear speech from this speaker to save their voice.
      </p>
    );
  }
  return (
    <div className="mt-4 border-t pt-3">
      <p className="text-xs font-medium">Remember this voice</p>
      <p className="text-xs text-muted-foreground">Earshot will recognise them in future meetings.</p>
      <Button
        size="xs"
        variant="outline"
        className="mt-1.5"
        disabled={!name}
        title={name ? undefined : "Give the speaker a name first"}
        onClick={() => run(profiles.save(s.id, name))}
      >
        <AudioLines aria-hidden />
        {name ? `Save voice as “${name}”` : "Name them first to save the voice"}
      </Button>
      {saved.length > 0 && (
        <>
          <p className="mt-2 text-xs text-muted-foreground">Or this is someone you've saved:</p>
          <div className="mt-1 flex flex-wrap gap-1.5">
            {saved.map((p) => (
              <Button key={p.id} size="xs" variant="outline" onClick={() => run(profiles.link(s.id, p.id))}>
                {p.name}
              </Button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
