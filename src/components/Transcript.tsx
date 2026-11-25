import { useEffect, useMemo, useRef, useState } from "react";
import { LocateFixed } from "lucide-react";
import { cn } from "cn";
import { SpeakerPopover } from "@/components/SpeakerPopover";
import { Button } from "@/components/ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { clock, meeting, speakerColor, speakerDisplayName, type Player, type Speaker } from "@/lib/meeting";
import type { Utterance } from "@/lib/models";

/** A highlightable span: one word, or a whole edited line (its word timings no longer match). */
interface Mark {
  start: number;
  end: number;
  line: number;
}

/** A word stays highlighted this long after it ends, so short gaps don't flicker. */
const HOLD_MS = 400;

const ACTIVE_WORD = ["bg-primary/15", "rounded-sm"];
const ACTIVE_LINE = ["bg-muted/60"];

function prefersReducedMotion() {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function Transcript({
  lines,
  speakers,
  meName,
  player,
  onChanged,
  onError,
}: {
  lines: Utterance[];
  speakers: Speaker[];
  meName: string;
  player: Player | null;
  onChanged: () => void;
  onError: (e: string) => void;
}) {
  const [editing, setEditing] = useState<number | null>(null);
  const [follow, setFollow] = useState(true);
  const followRef = useRef(true);
  followRef.current = follow;
  const lineEls = useRef<(HTMLLIElement | null)[]>([]);
  const markEls = useRef<(HTMLElement | null)[]>([]);
  const speakerById = useMemo(() => new Map(speakers.map((s) => [s.id, s])), [speakers]);

  // Every highlightable span in time order.
  const marks = useMemo(() => {
    const out: Mark[] = [];
    lines.forEach((u, line) => {
      if (u.edited || u.words.length === 0) out.push({ start: u.startMs, end: u.endMs, line });
      else for (const w of u.words) out.push({ start: w.startMs, end: w.endMs, line });
    });
    return out;
  }, [lines]);
  // Index of each line's first mark, to find its spans when rendering.
  const firstMark = useMemo(() => {
    const first: number[] = [];
    marks.forEach((m, i) => {
      if (first[m.line] === undefined) first[m.line] = i;
    });
    return first;
  }, [marks]);
  // Overlapping speakers make start times non-monotonic; search a sorted copy.
  const byStart = useMemo(
    () => marks.map((m, i) => ({ start: m.start, i })).sort((a, b) => a.start - b.start),
    [marks],
  );

  // Highlight the current word and line straight in the DOM (no React render per frame).
  // Depends on the stable `subscribe`, not the player object, which changes on every render.
  const subscribe = player?.subscribe;
  useEffect(() => {
    if (!subscribe) return;
    let activeMark = -1;
    let activeLine = -1;
    const set = (el: Element | null | undefined, cls: string[], on: boolean) => {
      if (el) for (const c of cls) el.classList.toggle(c, on);
    };
    const unsubscribe = subscribe((ms) => {
      // Last mark starting at or before ms.
      let lo = 0;
      let hi = byStart.length - 1;
      let found = -1;
      while (lo <= hi) {
        const mid = (lo + hi) >> 1;
        if (byStart[mid].start <= ms) {
          found = mid;
          lo = mid + 1;
        } else hi = mid - 1;
      }
      const idx = found >= 0 && ms <= marks[byStart[found].i].end + HOLD_MS ? byStart[found].i : -1;
      if (idx === activeMark) return;
      set(markEls.current[activeMark], ACTIVE_WORD, false);
      set(markEls.current[idx], ACTIVE_WORD, true);
      activeMark = idx;
      const line = idx >= 0 ? marks[idx].line : -1;
      if (line !== activeLine && line >= 0) {
        set(lineEls.current[activeLine], ACTIVE_LINE, false);
        set(lineEls.current[line], ACTIVE_LINE, true);
        activeLine = line;
        if (followRef.current) {
          lineEls.current[line]?.scrollIntoView({
            block: "center",
            behavior: prefersReducedMotion() ? "auto" : "smooth",
          });
        }
      }
    });
    return () => {
      unsubscribe();
      set(markEls.current[activeMark], ACTIVE_WORD, false);
      set(lineEls.current[activeLine], ACTIVE_LINE, false);
    };
  }, [subscribe, marks, byStart]);

  // Scrolling by hand stops following playback until the user asks for it again.
  useEffect(() => {
    const stop = () => setFollow(false);
    window.addEventListener("wheel", stop, { passive: true });
    window.addEventListener("touchmove", stop, { passive: true });
    return () => {
      window.removeEventListener("wheel", stop);
      window.removeEventListener("touchmove", stop);
    };
  }, []);

  const seekPlay = (ms: number) => {
    if (!player) return;
    player.seek(ms);
    setFollow(true);
    if (!player.playing) player.play();
  };

  const saveText = (u: Utterance, text: string) => {
    setEditing(null);
    if (text.trim() && text.trim() !== u.text) {
      meeting
        .setUtteranceText(u.id, text)
        .then(onChanged)
        .catch((e) => onError(String(e)));
    }
  };

  return (
    <div className="relative">
      <ol className="space-y-1 select-text" aria-label="Transcript">
        {lines.map((u, li) => {
          const sp = u.speakerId != null ? speakerById.get(u.speakerId) : undefined;
          const color = speakerColor(u.speakerLabel, u.isMe);
          const name = speakerDisplayName(
            { label: u.speakerLabel, name: u.speakerName, isMe: u.isMe },
            meName,
            u.trackKind === "mic" ? meName : "Meeting",
          );
          const fm = firstMark[li];
          const label = (
            <button
              type="button"
              className={cn("truncate text-left font-medium outline-none hover:underline focus-visible:underline", color.text)}
              title={sp ? `${name}: rename or merge` : name}
            >
              {name}
            </button>
          );
          return (
            <ContextMenu key={u.id}>
              <ContextMenuTrigger asChild>
                <li
                  ref={(el) => {
                    lineEls.current[li] = el;
                  }}
                  className="grid grid-cols-[3.5rem_8rem_1fr] gap-2 rounded-md px-2 py-1.5 text-sm transition-colors"
                >
                  <button
                    type="button"
                    onClick={() => seekPlay(u.startMs)}
                    className="self-start text-left text-muted-foreground tabular-nums outline-none hover:text-foreground focus-visible:underline"
                    title="Play from here"
                  >
                    {clock(u.startMs)}
                  </button>
                  {sp ? (
                    <SpeakerPopover speaker={sp} speakers={speakers} meName={meName} onChanged={onChanged} onError={onError}>
                      {label}
                    </SpeakerPopover>
                  ) : (
                    <span className={cn("truncate font-medium", color.text)}>{name}</span>
                  )}
                  {editing === u.id ? (
                    <EditBox initial={u.text} onSave={(t) => saveText(u, t)} onCancel={() => setEditing(null)} />
                  ) : u.edited || u.words.length === 0 ? (
                    <span
                      ref={(el) => {
                        markEls.current[fm] = el;
                      }}
                      className="cursor-pointer"
                      onClick={() => seekPlay(u.startMs)}
                    >
                      {u.text}
                    </span>
                  ) : (
                    <span>
                      {u.words.map((w, wi) => (
                        <span
                          key={wi}
                          ref={(el) => {
                            markEls.current[fm + wi] = el;
                          }}
                          className="cursor-pointer hover:underline hover:decoration-muted-foreground/50"
                          onClick={() => seekPlay(w.startMs)}
                        >
                          {wi === 0 ? w.text.trimStart() : w.text}
                        </span>
                      ))}
                    </span>
                  )}
                </li>
              </ContextMenuTrigger>
              <ContextMenuContent>
                <ContextMenuItem onSelect={() => seekPlay(u.startMs)}>Play from here</ContextMenuItem>
                {speakers.length > 1 && (
                  <ContextMenuSub>
                    <ContextMenuSubTrigger>Change speaker</ContextMenuSubTrigger>
                    <ContextMenuSubContent>
                      {speakers
                        .filter((s) => s.id !== u.speakerId)
                        .map((s) => (
                          <ContextMenuItem
                            key={s.id}
                            onSelect={() =>
                              meeting
                                .setUtteranceSpeaker(u.id, s.id)
                                .then(onChanged)
                                .catch((e) => onError(String(e)))
                            }
                          >
                            <span className={cn("size-2 rounded-full", speakerColor(s.label, s.isMe).bg)} aria-hidden />
                            {speakerDisplayName(s, meName)}
                          </ContextMenuItem>
                        ))}
                    </ContextMenuSubContent>
                  </ContextMenuSub>
                )}
                <ContextMenuItem onSelect={() => setEditing(u.id)}>Edit text</ContextMenuItem>
                <ContextMenuSeparator />
                <ContextMenuItem onSelect={() => navigator.clipboard.writeText(u.text)}>Copy line</ContextMenuItem>
              </ContextMenuContent>
            </ContextMenu>
          );
        })}
      </ol>

      {player && !follow && (
        <div className="sticky bottom-4 flex justify-center">
          <Button size="sm" variant="secondary" className="shadow-md" onClick={() => setFollow(true)}>
            <LocateFixed aria-hidden />
            Follow playback
          </Button>
        </div>
      )}
    </div>
  );
}

function EditBox({ initial, onSave, onCancel }: { initial: string; onSave: (t: string) => void; onCancel: () => void }) {
  const [text, setText] = useState(initial);
  // Closing the box blurs it; make sure Enter/Escape and the blur that follows act only once.
  const done = useRef(false);
  const finish = (save: boolean) => {
    if (done.current) return;
    done.current = true;
    if (save) onSave(text);
    else onCancel();
  };
  return (
    <textarea
      className="min-h-16 w-full resize-y rounded-md border border-input bg-transparent px-2 py-1 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
      value={text}
      autoFocus
      aria-label="Edit line"
      onChange={(e) => setText(e.target.value)}
      onBlur={() => finish(true)}
      onKeyDown={(e) => {
        if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          finish(true);
        } else if (e.key === "Escape") {
          e.preventDefault();
          finish(false);
        }
      }}
    />
  );
}
