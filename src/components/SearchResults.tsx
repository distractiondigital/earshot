import { Fragment, useEffect, useState } from "react";
import { clock, meeting as api, type SearchHit } from "@/lib/meeting";
import type { Meeting } from "@/lib/recorder";

/** Renders a snippet, highlighting the parts between \u0001 and \u0002. */
function Snippet({ text }: { text: string }) {
  const parts = text.split(/\u0001|\u0002/);
  return (
    <>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="rounded-sm bg-primary/15 px-0.5 text-foreground">
            {p}
          </mark>
        ) : (
          <Fragment key={i}>{p}</Fragment>
        ),
      )}
    </>
  );
}

/** Transcript and title matches for a query, grouped by meeting. */
export function SearchResults({
  query,
  meetings,
  onOpen,
}: {
  query: string;
  meetings: Meeting[];
  onOpen: (id: number, atMs?: number) => void;
}) {
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let stale = false;
    const t = setTimeout(() => {
      api
        .search(query)
        .then((h) => !stale && (setHits(h), setError(null)))
        .catch((e) => !stale && setError(String(e)));
    }, 200);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [query]);

  const q = query.trim().toLowerCase();
  const byId = new Map(meetings.map((m) => [m.id, m]));
  // Meetings in list order: title/tag matches and transcript matches together.
  const groups = meetings
    .map((m) => ({
      meeting: m,
      titleMatch: !!q && (m.title.toLowerCase().includes(q) || m.tags.some((t) => t.toLowerCase().includes(q))),
      hits: (hits ?? []).filter((h) => h.meetingId === m.id),
    }))
    .filter((g) => g.titleMatch || g.hits.length > 0);

  if (error) return <p className="mt-6 text-sm text-destructive">{error}</p>;
  if (hits === null) return <p className="mt-6 text-sm text-muted-foreground">Searching…</p>;
  if (groups.length === 0) return <p className="mt-6 text-sm text-muted-foreground">Nothing found for “{query}”.</p>;

  return (
    <ul className="mt-4 divide-y divide-border border-t border-border" aria-label="Search results">
      {groups.map(({ meeting: m, hits }) => {
        const date = new Date(m.startedAt);
        return (
          <li key={m.id} className="py-3">
            <button type="button" onClick={() => onOpen(m.id)} className="text-left outline-none focus-visible:underline">
              <span className="text-sm font-medium">{byId.get(m.id)?.title || "Untitled meeting"}</span>
              <span className="ml-2 text-xs text-muted-foreground">
                {date.toLocaleDateString(undefined, { dateStyle: "medium" })}
              </span>
            </button>
            {hits.length > 0 && (
              <ul className="mt-1 space-y-0.5">
                {hits.slice(0, 5).map((h) => (
                  <li key={h.utteranceId}>
                    <button
                      type="button"
                      onClick={() => onOpen(m.id, h.startMs)}
                      className="flex w-full gap-3 rounded-md px-2 py-1 text-left text-sm outline-none hover:bg-muted focus-visible:bg-muted"
                    >
                      <span className="w-12 shrink-0 text-xs leading-5 text-muted-foreground tabular-nums">
                        {clock(h.startMs)}
                      </span>
                      <span className="min-w-0 text-muted-foreground">
                        <Snippet text={h.snippet} />
                      </span>
                    </button>
                  </li>
                ))}
                {hits.length > 5 && (
                  <li className="px-2 text-xs text-muted-foreground">
                    and {hits.length - 5} more in this meeting
                  </li>
                )}
              </ul>
            )}
          </li>
        );
      })}
    </ul>
  );
}
