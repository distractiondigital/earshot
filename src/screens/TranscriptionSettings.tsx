import { useEffect, useState } from "react";
import { Check, ChevronDown, Cpu, Download, Trash2, X } from "lucide-react";
import { cn } from "cn";
import { SettingsRow } from "@/components/SettingsRow";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { ipc } from "@/lib/ipc";
import { formatBytes, models, useModels, type Hardware, type ModelView } from "@/lib/models";

const LANGUAGES: [string, string][] = [
  ["auto", "Detect automatically"],
  ["en", "English"],
  ["es", "Spanish"],
  ["fr", "French"],
  ["de", "German"],
  ["it", "Italian"],
  ["pt", "Portuguese"],
  ["nl", "Dutch"],
  ["pl", "Polish"],
  ["ru", "Russian"],
  ["uk", "Ukrainian"],
  ["tr", "Turkish"],
  ["ar", "Arabic"],
  ["hi", "Hindi"],
  ["ja", "Japanese"],
  ["ko", "Korean"],
  ["zh", "Chinese"],
];

export function TranscriptionSettings() {
  const { info, progress, errors } = useModels();
  const [language, setLanguage] = useState("auto");
  const [backend, setBackend] = useState("auto");
  const [auto, setAuto] = useState(true);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    ipc.getSetting("transcription.language").then((v) => v && setLanguage(v));
    ipc.getSetting("transcription.backend").then((v) => v && setBackend(v));
    ipc.getSetting("transcription.autoProcess").then((v) => setAuto(v !== "false"));
  }, []);

  const save = (key: string, value: string) => ipc.setSetting(key, value).catch((e) => setError(String(e)));
  const transcription = info?.models.filter((m) => m.category === "transcription") ?? [];
  // English-only models only matter when the language is English.
  const visible = transcription.filter((m) => !m.englishOnly || language === "en");
  const standard = visible.filter((m) => !m.advanced);
  const advanced = visible.filter((m) => m.advanced);
  const selected = transcription.find((m) => m.selected);
  const gpuName = info?.hardware.workerGpu === "cuda" ? "CUDA" : info?.hardware.workerGpu === "metal" ? "Metal" : null;

  const card = (m: ModelView) => (
    <ModelCard
      key={m.id}
      model={m}
      downloaded={progress[m.id] ?? m.downloadedBytes}
      error={errors[m.id]}
      onError={setError}
    />
  );

  return (
    <div>
      {info && <HardwareLine hw={info.hardware} />}

      <div className="mt-4 grid gap-2">{standard.map(card)}</div>

      {advanced.length > 0 && (
        <div className="mt-3">
          <button
            type="button"
            onClick={() => setShowAdvanced((v) => !v)}
            aria-expanded={showAdvanced}
            className="flex items-center gap-1 text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:underline"
          >
            <ChevronDown className={cn("size-4 transition-transform", showAdvanced && "rotate-180")} aria-hidden />
            Advanced models ({advanced.length})
          </button>
          {showAdvanced && <div className="mt-2 grid gap-2">{advanced.map(card)}</div>}
        </div>
      )}

      <p className="mt-3 text-sm text-muted-foreground" role="status">
        {selected ? `Using ${selected.name} for transcription.` : "No transcription model selected yet."}
      </p>

      <div className="mt-4">
        <SettingsRow label="Language">
          <Select
            value={language}
            onValueChange={(v) => {
              setLanguage(v);
              save("transcription.language", v);
            }}
          >
            <SelectTrigger className="w-56" aria-label="Language">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {LANGUAGES.map(([code, name]) => (
                <SelectItem key={code} value={code}>
                  {name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingsRow>
        <SettingsRow label="Compute" hint="Auto uses the GPU when available and falls back to the CPU if it fails.">
          <Select
            value={backend}
            onValueChange={(v) => {
              setBackend(v);
              save("transcription.backend", v);
            }}
          >
            <SelectTrigger className="w-56" aria-label="Compute backend">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="auto">Auto</SelectItem>
              {gpuName && <SelectItem value="gpu">GPU ({gpuName})</SelectItem>}
              <SelectItem value="cpu">CPU</SelectItem>
            </SelectContent>
          </Select>
        </SettingsRow>
        <SettingsRow label="Transcribe automatically after recording">
          <Switch
            checked={auto}
            onCheckedChange={(v) => {
              setAuto(v);
              save("transcription.autoProcess", String(v));
            }}
            aria-label="Transcribe automatically after recording"
          />
        </SettingsRow>
      </div>

      {error && (
        <p className="mt-3 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function HardwareLine({ hw }: { hw: Hardware }) {
  const gpu = hw.nvidia
    ? `${hw.nvidia.name} (${formatBytes(hw.nvidia.vramBytes)})`
    : hw.appleSilicon
      ? `Apple Silicon (${formatBytes(hw.ramBytes)} unified memory)`
      : null;
  const accel = hw.workerGpu === "cuda" ? "CUDA" : hw.workerGpu === "metal" ? "Metal" : null;
  return (
    <p className="flex items-center gap-2 text-sm text-muted-foreground">
      <Cpu className="size-4" aria-hidden />
      {gpu ?? hw.cpu}
      {accel ? ` · GPU acceleration: ${accel}` : " · CPU only"}
    </p>
  );
}

function Dots({ value, label }: { value: number; label: string }) {
  return (
    <span className="inline-flex items-center gap-1" aria-label={`${label} ${value} of 5`}>
      <span className="text-muted-foreground">{label}</span>
      <span aria-hidden className="tracking-tighter">
        {"●".repeat(value)}
        <span className="text-muted-foreground/40">{"●".repeat(5 - value)}</span>
      </span>
    </span>
  );
}

function ModelCard({
  model: m,
  downloaded,
  error,
  onError,
}: {
  model: ModelView;
  downloaded: number;
  error?: string;
  onError: (e: string) => void;
}) {
  const run = (p: Promise<unknown>) => p.catch((e) => onError(String(e)));
  const pct = m.sizeBytes ? Math.min(100, (downloaded / m.sizeBytes) * 100) : 0;
  return (
    <div
      className={cn(
        "rounded-lg border p-3",
        m.selected ? "border-foreground/60 bg-muted/40" : "border-border",
      )}
    >
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-sm font-medium">{m.name}</span>
            {m.recommended && <Badge>Recommended for this PC</Badge>}
            {m.badge && !m.recommended && <Badge variant="secondary">{m.badge}</Badge>}
            {m.englishOnly && <Badge variant="outline">English only</Badge>}
          </div>
          <p className="mt-0.5 text-xs text-muted-foreground">{m.description}</p>
          <div className="mt-1.5 flex flex-wrap gap-x-4 text-xs">
            <span className="text-muted-foreground">{formatBytes(m.sizeBytes)}</span>
            <Dots value={m.accuracy} label="Accuracy" />
            <Dots value={m.speed} label="Speed" />
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {m.state === "available" && (
            <Button size="sm" variant="outline" onClick={() => run(models.download(m.id))}>
              <Download aria-hidden />
              Download
            </Button>
          )}
          {m.state === "downloading" && (
            <Button size="sm" variant="ghost" onClick={() => run(models.cancelDownload(m.id))}>
              <X aria-hidden />
              Cancel
            </Button>
          )}
          {m.state === "ready" && m.selected && (
            <span className="flex items-center gap-1 px-2 text-sm font-medium">
              <Check className="size-4" aria-hidden />
              Selected
            </span>
          )}
          {m.state === "ready" && !m.selected && (
            <>
              <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
                <span className="size-2 rounded-full bg-green-500" aria-hidden />
                Ready
              </span>
              <Button size="sm" variant="outline" onClick={() => run(models.select(m.id))}>
                Use
              </Button>
            </>
          )}
          {m.state === "ready" && (
            <Button
              size="icon-sm"
              variant="ghost"
              aria-label={`Delete ${m.name}`}
              onClick={() => run(models.remove(m.id))}
            >
              <Trash2 aria-hidden />
            </Button>
          )}
        </div>
      </div>
      {m.state === "downloading" && (
        <div className="mt-2 flex items-center gap-3">
          <Progress value={pct} className="flex-1" aria-label={`Downloading ${m.name}`} />
          <span className="w-28 text-right text-xs text-muted-foreground tabular-nums">
            {formatBytes(downloaded)} / {formatBytes(m.sizeBytes)}
          </span>
        </div>
      )}
      {error && <p className="mt-2 text-xs text-destructive">{error}</p>}
    </div>
  );
}
