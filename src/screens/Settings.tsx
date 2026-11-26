import { useEffect, useState } from "react";
import { SettingsRow } from "@/components/SettingsRow";
import icon from "@/assets/icon.png";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { aboutNote, credits } from "@/lib/credits";
import { ipc, type AppInfo, type Theme } from "@/lib/ipc";
import { useTheme } from "@/lib/theme";
import { useUpdates } from "@/lib/updates";
import { RecordingSettings } from "@/screens/RecordingSettings";
import { SpeakersSettings } from "@/screens/SpeakersSettings";
import { SummarySettings } from "@/screens/SummarySettings";
import { TranscriptionSettings } from "@/screens/TranscriptionSettings";

export function Settings({ tab, onTab }: { tab: string; onTab: (t: string) => void }) {
  return (
    <div className="mx-auto max-w-3xl p-8">
      <h1 className="text-lg font-semibold">Settings</h1>
      <Tabs value={tab} onValueChange={onTab} className="mt-6">
        <TabsList>
          <TabsTrigger value="general">General</TabsTrigger>
          <TabsTrigger value="recording">Recording</TabsTrigger>
          <TabsTrigger value="transcription">Transcription</TabsTrigger>
          <TabsTrigger value="speakers">Speakers</TabsTrigger>
          <TabsTrigger value="summary">Summary</TabsTrigger>
          <TabsTrigger value="about">About</TabsTrigger>
        </TabsList>
        <TabsContent value="general" className="pt-4">
          <General />
        </TabsContent>
        <TabsContent value="recording" className="pt-4">
          <RecordingSettings />
        </TabsContent>
        <TabsContent value="transcription" className="pt-4">
          <TranscriptionSettings />
        </TabsContent>
        <TabsContent value="speakers" className="pt-4">
          <SpeakersSettings />
        </TabsContent>
        <TabsContent value="summary" className="pt-4">
          <SummarySettings />
        </TabsContent>
        <TabsContent value="about" className="pt-4">
          <About />
        </TabsContent>
      </Tabs>
    </div>
  );
}

const Row = SettingsRow;

function General() {
  const { theme, setTheme } = useTheme();
  return (
    <Row label="Theme">
      <ToggleGroup
        type="single"
        variant="outline"
        size="sm"
        value={theme}
        onValueChange={(v) => v && setTheme(v as Theme)}
        aria-label="Theme"
      >
        <ToggleGroupItem value="light">Light</ToggleGroupItem>
        <ToggleGroupItem value="dark">Dark</ToggleGroupItem>
        <ToggleGroupItem value="system">System</ToggleGroupItem>
      </ToggleGroup>
    </Row>
  );
}

function About() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    ipc.appInfo().then(setInfo).catch((e) => setError(String(e)));
  }, []);
  if (error) return <p className="text-sm text-destructive">{error}</p>;
  if (!info) return null;
  return (
    <div>
      <header className="flex items-center gap-4 pb-4">
        <img src={icon} alt="" className="size-12 rounded-lg" />
        <div>
          <h2 className="text-lg font-semibold">Earshot</h2>
          <p className="text-sm text-muted-foreground">Version {info.version}</p>
        </div>
      </header>

      <Updates />
      <Row label="Transcription engine">
        <span className="text-sm text-muted-foreground">{info.workerVersion ?? "not available"}</span>
      </Row>
      <Row label="Database schema">
        <span className="text-sm text-muted-foreground">v{info.schemaVersion}</span>
      </Row>
      <Row label="Data folder">
        <span className="truncate text-sm text-muted-foreground select-text" title={info.dataDir}>
          {info.dataDir}
        </span>
      </Row>

      {credits.map((group) => (
        <section key={group.title} className="mt-8" aria-labelledby={`credits-${group.title}`}>
          <h3
            id={`credits-${group.title}`}
            className="text-xs font-medium tracking-wide text-muted-foreground uppercase"
          >
            {group.title}
          </h3>
          <ul className="mt-3 space-y-3">
            {group.items.map((c) => (
              <li key={c.name} className="text-sm">
                <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
                  <span className="font-medium">{c.name}</span>
                  <span className="text-muted-foreground">by {c.by}</span>
                  <Badge variant="outline" className="font-normal">
                    {c.license}
                  </Badge>
                </div>
                {c.note && <p className="text-muted-foreground">{c.note}</p>}
                <button
                  type="button"
                  onClick={() => ipc.openLink(c.url).catch(() => {})}
                  className="text-xs text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
                >
                  {c.url.replace(/^https:\/\//, "")}
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}

      <p className="mt-8 text-sm text-muted-foreground">{aboutNote}</p>
      <p className="mt-4 border-t border-border pt-4 text-sm text-muted-foreground">
        © 2026 Distraction Digital. Free and open source (MIT).
      </p>
    </div>
  );
}

function Updates() {
  const { enabled, autoCheck, setAutoCheck, status, checkNow, install } = useUpdates();
  if (!enabled) {
    return (
      <Row label="Updates">
        <span className="text-sm text-muted-foreground">Disabled in development builds</span>
      </Row>
    );
  }
  const busy = status.kind === "checking" || status.kind === "downloading" || status.kind === "installing";
  let message = "";
  if (status.kind === "checking") message = "Checking…";
  else if (status.kind === "upToDate") message = "Earshot is up to date.";
  else if (status.kind === "available") message = `Version ${status.update.version} is available.`;
  else if (status.kind === "downloading") message = "Downloading…";
  else if (status.kind === "installing") message = "Installing…";
  else if (status.kind === "error") message = "Couldn't check for updates.";

  return (
    <>
      <Row label="Check for updates automatically">
        <Switch checked={autoCheck} onCheckedChange={setAutoCheck} aria-label="Check for updates automatically" />
      </Row>
      <Row label="Updates">
        <div className="flex items-center gap-3">
          <span
            className="text-sm text-muted-foreground"
            role="status"
            title={status.kind === "error" ? status.message : undefined}
          >
            {message}
          </span>
          {status.kind === "available" ? (
            <Button size="sm" onClick={install}>
              Update and restart
            </Button>
          ) : (
            <Button size="sm" variant="outline" onClick={checkNow} disabled={busy}>
              Check now
            </Button>
          )}
        </div>
      </Row>
    </>
  );
}
