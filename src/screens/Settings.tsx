import { useEffect, useState, type ReactNode } from "react";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { ipc, type AppInfo, type Theme } from "@/lib/ipc";
import { useTheme } from "@/lib/theme";

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
    </div>
  );
}
