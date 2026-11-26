import { AudioLines } from "lucide-react";
import { cn } from "cn";
import { SpeakerPopover } from "@/components/SpeakerPopover";
import { speakerColor, speakerDisplayName, type Speaker } from "@/lib/meeting";

export function SpeakersPanel({
  speakers,
  meName,
  onChanged,
  onError,
}: {
  speakers: Speaker[];
  meName: string;
  onChanged: () => void;
  onError: (e: string) => void;
}) {
  const total = speakers.reduce((n, s) => n + s.talkMs, 0) || 1;
  return (
    <section aria-labelledby="speakers-heading">
      <h2 id="speakers-heading" className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
        Speakers
      </h2>
      {speakers.length === 0 ? (
        <p className="mt-2 text-sm text-muted-foreground">No speakers yet.</p>
      ) : (
        <ul className="mt-2 space-y-1">
          {speakers.map((s) => {
            const share = Math.round((s.talkMs / total) * 100);
            return (
              <li key={s.id}>
                <SpeakerPopover speaker={s} speakers={speakers} meName={meName} onChanged={onChanged} onError={onError} align="end">
                  <button
                    type="button"
                    className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none hover:bg-muted focus-visible:bg-muted"
                    title={s.isMe ? "Your microphone" : "Rename or merge"}
                  >
                    <span className={cn("size-2.5 shrink-0 rounded-full", speakerColor(s.label, s.isMe).bg)} aria-hidden />
                    <span className="min-w-0 flex-1 truncate">
                      {speakerDisplayName(s, meName)}
                      {(s.name || s.profileName) && !s.isMe && !s.autoMatched && (
                        <span className="ml-1 text-xs text-muted-foreground">({s.label})</span>
                      )}
                    </span>
                    {s.autoMatched ? (
                      <span
                        className="rounded-sm border border-dashed px-1 text-[10px] text-muted-foreground"
                        title={`Recognised as ${s.profileName}. Click to confirm or reject.`}
                      >
                        Suggested
                      </span>
                    ) : (
                      s.profileId && (
                        <AudioLines className="size-3.5 text-muted-foreground" aria-label="Saved voice" />
                      )
                    )}
                    <span className="text-xs text-muted-foreground tabular-nums" title={`${s.utterances} lines`}>
                      {share}%
                    </span>
                  </button>
                </SpeakerPopover>
              </li>
            );
          })}
        </ul>
      )}
      <p className="mt-3 text-xs text-muted-foreground">
        Click a speaker to name them, save their voice, or merge them with another. Right-click a line to change its
        speaker.
      </p>
    </section>
  );
}
