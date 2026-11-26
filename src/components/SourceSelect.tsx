import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectSeparator, SelectTrigger, SelectValue } from "@/components/ui/select";
import type { Sources, SystemSource } from "@/lib/recorder";

/** Parses the saved default: "all" / "none" / JSON SystemSource. */
export function parseSavedSource(v: string | null): SystemSource {
  if (!v || v === "all") return { kind: "all" };
  if (v === "none") return { kind: "none" };
  try {
    return JSON.parse(v) as SystemSource;
  } catch {
    return { kind: "all" };
  }
}

export function serializeSource(s: SystemSource): string {
  return s.kind === "all" || s.kind === "none" ? s.kind : JSON.stringify(s);
}

// Apps are keyed by name (PIDs change between launches); devices by id.
function keyOf(s: SystemSource): string {
  switch (s.kind) {
    case "all":
    case "none":
      return s.kind;
    case "output":
      return `out:${s.id}`;
    case "input":
      return `in:${s.id}`;
    case "app":
      return `app:${s.name}`;
  }
}

/**
 * Picker for the meeting-audio track: the default output device, one output device (what it plays),
 * one input device, one app, or none (microphone only).
 */
export function SourceSelect({
  value,
  onChange,
  sources,
  onOpen,
  className = "w-64",
}: {
  value: SystemSource;
  onChange: (s: SystemSource) => void;
  sources: Sources | null;
  onOpen?: () => void;
  className?: string;
}) {
  const outputs = sources?.outputDevices ?? [];
  const inputs = sources?.inputDevices ?? [];
  const apps = sources?.supportsAppCapture ? [...new Map(sources.apps.map((a) => [a.name, a])).values()] : [];

  // Keep the current choice visible even if it is not in the live lists (unplugged / not running).
  const current = keyOf(value);
  const missing =
    (value.kind === "output" && !outputs.some((d) => d.id === value.id)) ||
    (value.kind === "input" && !inputs.some((d) => d.id === value.id)) ||
    (value.kind === "app" && !apps.some((a) => a.name === value.name));

  const fromKey = (key: string): SystemSource => {
    if (key === current) return value;
    if (key === "all" || key === "none") return { kind: key };
    if (key.startsWith("out:")) {
      const d = outputs.find((o) => `out:${o.id}` === key)!;
      return { kind: "output", id: d.id, name: d.name };
    }
    if (key.startsWith("in:")) {
      const d = inputs.find((i) => `in:${i.id}` === key)!;
      return { kind: "input", id: d.id, name: d.name };
    }
    const a = apps.find((x) => `app:${x.name}` === key)!;
    return { kind: "app", pid: a.pid, name: a.name };
  };

  return (
    <Select value={current} onValueChange={(k) => onChange(fromKey(k))} onOpenChange={(o) => o && onOpen?.()}>
      <SelectTrigger className={className} aria-label="Meeting audio source">
        <SelectValue />
      </SelectTrigger>
      {/* Opens below the button: the lists refresh while open (devices come and go), which would make
          the default item-aligned menu jump. */}
      <SelectContent position="popper">
        <SelectItem value="all">Default system audio</SelectItem>
        <SelectItem value="none">Microphone only</SelectItem>
        {missing && "name" in value && (
          <SelectItem value={current}>
            {value.name} {value.kind === "app" ? "(not running)" : "(not connected)"}
          </SelectItem>
        )}
        {outputs.length > 0 && (
          <>
            <SelectSeparator />
            <SelectGroup>
              <SelectLabel>What an output device plays</SelectLabel>
              {outputs.map((d) => (
                <SelectItem key={d.id} value={`out:${d.id}`}>
                  {d.name}
                </SelectItem>
              ))}
            </SelectGroup>
          </>
        )}
        {inputs.length > 0 && (
          <>
            <SelectSeparator />
            <SelectGroup>
              <SelectLabel>An input device</SelectLabel>
              {inputs.map((d) => (
                <SelectItem key={d.id} value={`in:${d.id}`}>
                  {d.name}
                </SelectItem>
              ))}
            </SelectGroup>
          </>
        )}
        {apps.length > 0 && (
          <>
            <SelectSeparator />
            <SelectGroup>
              <SelectLabel>Only this app</SelectLabel>
              {apps.map((a) => (
                <SelectItem key={a.name} value={`app:${a.name}`}>
                  {a.name}
                </SelectItem>
              ))}
            </SelectGroup>
          </>
        )}
      </SelectContent>
    </Select>
  );
}
