import { useEffect, useState } from "react";
import { Download } from "lucide-react";
import { SettingsRow } from "@/components/SettingsRow";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { ipc } from "@/lib/ipc";
import { formatBytes, models, useModels, type ModelView } from "@/lib/models";
import { ModelCard } from "@/screens/TranscriptionSettings";

export const DEFAULT_ME_NAME = "Me";
const DEFAULT_SENSITIVITY = 50;
/** Voice matching: the slider runs from lenient to strict over these similarity thresholds. */
const MATCH_LENIENT = 0.45;
const MATCH_STRICT = 0.75;
const DEFAULT_MATCH = 0.6;
const toStrictness = (t: number) => Math.round(((t - MATCH_LENIENT) / (MATCH_STRICT - MATCH_LENIENT)) * 100);
const toThreshold = (v: number) => MATCH_LENIENT + ((MATCH_STRICT - MATCH_LENIENT) * v) / 100;

export function SpeakersSettings() {
  const { info, progress, errors } = useModels();
  const [meName, setMeName] = useState(DEFAULT_ME_NAME);
  const [sensitivity, setSensitivity] = useState(DEFAULT_SENSITIVITY);
  const [autoMatch, setAutoMatch] = useState(true);
  const [strictness, setStrictness] = useState(toStrictness(DEFAULT_MATCH));
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ipc.getSetting("speakers.meName").then((v) => v && setMeName(v));
    ipc.getSetting("speakers.autoMatch").then((v) => setAutoMatch(v !== "false"));
    ipc.getSetting("speakers.matchThreshold").then((v) => {
      const n = Number(v);
      if (v && Number.isFinite(n)) setStrictness(Math.min(100, Math.max(0, toStrictness(n))));
    });
    ipc.getSetting("speakers.sensitivity").then((v) => {
      const n = Number(v);
      if (v && Number.isFinite(n)) setSensitivity(n);
    });
  }, []);

  const save = (key: string, value: string) =>
    ipc
      .setSetting(key, value)
      .then(() => setError(null))
      .catch((e) => setError(String(e)));

  const embeddings = info?.models.filter((m) => m.category === "embedding") ?? [];
  const segmentation = info?.models.find((m) => m.category === "segmentation");

  return (
    <div>
      <SettingsRow label="Your name" hint="Shown for everything recorded by your microphone.">
        <Input
          className="w-56"
          value={meName}
          maxLength={60}
          aria-label="Your name"
          onChange={(e) => setMeName(e.target.value)}
          onBlur={() => {
            const name = meName.trim() || DEFAULT_ME_NAME;
            setMeName(name);
            save("speakers.meName", name);
          }}
        />
      </SettingsRow>
      <SettingsRow
        label="Sensitivity"
        hint="Raise it if different people get merged into one speaker; lower it if one person is split into several. Applies to meetings transcribed from now on."
      >
        <div className="w-56">
          <Slider
            min={0}
            max={100}
            step={5}
            value={[sensitivity]}
            onValueChange={([v]) => setSensitivity(v)}
            onValueCommit={([v]) => save("speakers.sensitivity", String(v))}
            aria-label="Speaker sensitivity"
          />
          <div className="mt-1.5 flex justify-between text-xs whitespace-nowrap text-muted-foreground">
            <span>Fewer speakers</span>
            <span>More speakers</span>
          </div>
        </div>
      </SettingsRow>
      <SettingsRow label="Recognise saved voices" hint="Suggests names for people whose voices you've saved.">
        <Switch
          checked={autoMatch}
          onCheckedChange={(v) => {
            setAutoMatch(v);
            save("speakers.autoMatch", String(v));
          }}
          aria-label="Recognise saved voices"
        />
      </SettingsRow>
      {autoMatch && (
        <SettingsRow
          label="Matching"
          hint="Stricter avoids wrong names but may miss someone on a bad line; more lenient does the opposite."
        >
          <div className="w-56">
            <Slider
              min={0}
              max={100}
              step={5}
              value={[strictness]}
              onValueChange={([v]) => setStrictness(v)}
              onValueCommit={([v]) => save("speakers.matchThreshold", toThreshold(v).toFixed(3))}
              aria-label="Voice matching strictness"
            />
            <div className="mt-1.5 flex justify-between text-xs whitespace-nowrap text-muted-foreground">
              <span>Lenient</span>
              <span>Strict</span>
            </div>
          </div>
        </SettingsRow>
      )}

      <h3 className="mt-6 text-sm font-medium">Voice model</h3>
      <p className="mt-1 text-xs text-muted-foreground">
        Tells voices apart in the meeting audio. Speakers are detected on this computer; nothing is uploaded.
      </p>
      <div className="mt-3 grid gap-2">
        {embeddings.map((m) => (
          <ModelCard
            key={m.id}
            model={m}
            downloaded={progress[m.id] ?? m.downloadedBytes}
            error={errors[m.id]}
            onError={setError}
          />
        ))}
      </div>

      {segmentation && (
        <SegmentationLine model={segmentation} downloaded={progress[segmentation.id] ?? segmentation.downloadedBytes} />
      )}

      {error && (
        <p className="mt-3 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** The single speech-turn model: only worth a line, since there's nothing to choose. */
function SegmentationLine({ model: m, downloaded }: { model: ModelView; downloaded: number }) {
  return (
    <p className="mt-3 flex items-center gap-2 text-xs text-muted-foreground" role="status">
      {m.state === "ready" && <>Speech-turn model ({m.name}): ready.</>}
      {m.state === "downloading" && (
        <>
          Speech-turn model: downloading {formatBytes(downloaded)} of {formatBytes(m.sizeBytes)}…
        </>
      )}
      {m.state === "available" && (
        <>
          Speech-turn model ({formatBytes(m.sizeBytes)}) isn't downloaded.
          <Button size="xs" variant="outline" onClick={() => models.download(m.id)}>
            <Download aria-hidden />
            Download
          </Button>
        </>
      )}
    </p>
  );
}
