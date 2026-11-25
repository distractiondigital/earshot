import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { ipc, type AppInfo, type Theme } from "@/lib/ipc";
import { useTheme } from "@/lib/theme";
import { useUpdates } from "@/lib/updates";

export function Settings() {
  return (
    <div className="mx-auto max-w-3xl p-8">
      <h1 className="text-lg font-semibold">Settings</h1>
      <Tabs defaultValue="general" className="mt-6">
        <TabsList>
          <TabsTrigger value="general">General</TabsTrigger>
          <TabsTrigger value="about">About</TabsTrigger>
        </TabsList>
        <TabsContent value="general" className="pt-4">
          <General />
        </TabsContent>
        <TabsContent value="about" className="pt-4">
          <About />
        </TabsContent>
      </Tabs>
    </div>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-border py-3 last:border-0">
      <span className="text-sm">{label}</span>
      {children}
    </div>
  );
}

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
      <Row label="Version">
        <span className="text-sm text-muted-foreground">{info.version}</span>
      </Row>
      <Row label="Database schema">
        <span className="text-sm text-muted-foreground">v{info.schemaVersion}</span>
      </Row>
      <Row label="Worker">
        <span className="text-sm text-muted-foreground">{info.workerVersion ?? "not available"}</span>
      </Row>
      <Row label="Data folder">
        <span className="truncate text-sm text-muted-foreground select-text" title={info.dataDir}>
          {info.dataDir}
        </span>
      </Row>
      <Updates />
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
