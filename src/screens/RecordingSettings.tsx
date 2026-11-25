import { useEffect, useState, type KeyboardEvent } from "react";
import { FolderOpen } from "lucide-react";
import { SettingsRow } from "@/components/SettingsRow";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { ipc } from "@/lib/ipc";
import { recorder, type InputDevice } from "@/lib/recorder";

const DEFAULT_DEVICE = "__default__";
const isMac = navigator.userAgent.includes("Mac");

export function RecordingSettings() {
  const [devices, setDevices] = useState<InputDevice[]>([]);
  const [mic, setMic] = useState(DEFAULT_DEVICE);
  const [system, setSystem] = useState("all");
  const [showPill, setShowPill] = useState(true);
  const [consent, setConsent] = useState(false);
  const [dir, setDir] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    recorder.sources().then((s) => setDevices(s.inputDevices)).catch(() => {});
    ipc.getSetting("recording.micDevice").then((v) => v && setMic(v));
    ipc.getSetting("recording.systemSource").then((v) => v && setSystem(v));
    ipc.getSetting("recording.showPill").then((v) => setShowPill(v !== "false"));
    ipc.getSetting("recording.consentReminder").then((v) => setConsent(v === "true"));
    recorder.recordingsDir().then(setDir);
  }, []);

  const save = (key: string, value: string) => ipc.setSetting(key, value).catch((e) => setError(String(e)));
  const savedMicMissing = mic !== DEFAULT_DEVICE && devices.length > 0 && !devices.some((d) => d.id === mic);

  return (
    <div>
      <SettingsRow
        label="Microphone"
        hint={savedMicMissing ? "The saved microphone isn't connected; the default will be used." : undefined}
      >
        <Select
          value={mic}
          onValueChange={(v) => {
            setMic(v);
            save("recording.micDevice", v === DEFAULT_DEVICE ? "" : v);
          }}
        >
          <SelectTrigger className="w-64" aria-label="Microphone">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={DEFAULT_DEVICE}>System default</SelectItem>
            {devices.map((d) => (
              <SelectItem key={d.id} value={d.id}>
                {d.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsRow>

      <SettingsRow label="Meeting audio" hint="Default for the hotkey and tray. Pick a single app when you start recording.">
        <Select
          value={system}
          onValueChange={(v) => {
            setSystem(v);
            save("recording.systemSource", v);
          }}
        >
          <SelectTrigger className="w-64" aria-label="Meeting audio">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All system audio</SelectItem>
            <SelectItem value="none">Don't record (microphone only)</SelectItem>
          </SelectContent>
        </Select>
      </SettingsRow>

      <SettingsRow label="Recordings folder" hint={dir}>
        <div className="flex shrink-0 gap-2">
          <Button variant="outline" size="sm" onClick={() => recorder.openRecordingsDir().catch((e) => setError(String(e)))}>
            <FolderOpen aria-hidden />
            Open
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              recorder
                .chooseRecordingsDir()
                .then((d) => d && setDir(d))
                .catch((e) => setError(String(e)))
            }
          >
            Change…
          </Button>
        </div>
      </SettingsRow>

      <SettingsRow label="Start/stop shortcut" hint="Works from any app.">
        <HotkeyField onError={setError} />
      </SettingsRow>

      <SettingsRow label="Show floating recorder while recording">
        <Switch
          checked={showPill}
          onCheckedChange={(v) => {
            setShowPill(v);
            save("recording.showPill", String(v));
          }}
          aria-label="Show floating recorder while recording"
        />
      </SettingsRow>

      <SettingsRow label="Remind me to tell participants I'm recording">
        <Switch
          checked={consent}
          onCheckedChange={(v) => {
            setConsent(v);
            save("recording.consentReminder", String(v));
          }}
          aria-label="Remind me to tell participants I'm recording"
        />
      </SettingsRow>

      {error && (
        <p className="mt-3 text-sm text-destructive" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** Shows "CommandOrControl+Shift+R" as Ctrl+Shift+R / ⌘⇧R. */
function displayHotkey(h: string) {
  if (!h) return "None";
  const parts = h.split("+");
  if (isMac) {
    const map: Record<string, string> = { CommandOrControl: "⌘", Command: "⌘", Control: "⌃", Shift: "⇧", Alt: "⌥" };
    return parts.map((p) => map[p] ?? p).join("");
  }
  return parts.map((p) => (p === "CommandOrControl" ? "Ctrl" : p)).join("+");
}

function HotkeyField({ onError }: { onError: (e: string | null) => void }) {
  const [hotkey, setHotkey] = useState("");
  const [capturing, setCapturing] = useState(false);

  useEffect(() => {
    recorder.getHotkey().then(setHotkey);
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    e.preventDefault();
    if (e.key === "Escape") return setCapturing(false);
    // Wait for a non-modifier key.
    if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return;
    let key: string | null = null;
    if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
    else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
    else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(e.code)) key = e.code;
    if (!key) return;
    const mods = [];
    if (e.ctrlKey || e.metaKey) mods.push("CommandOrControl");
    if (e.altKey) mods.push("Alt");
    if (e.shiftKey) mods.push("Shift");
    if (mods.length === 0 && !key.startsWith("F")) {
      onError("Use at least one modifier key (Ctrl, Alt or Shift).");
      return;
    }
    const next = [...mods, key].join("+");
    setCapturing(false);
    recorder
      .setHotkey(next)
      .then(() => {
        setHotkey(next);
        onError(null);
      })
      .catch((err) => onError(String(err)));
  };

  return (
    <Button
      variant="outline"
      size="sm"
      className="min-w-32 font-mono"
      onClick={() => setCapturing(true)}
      onBlur={() => setCapturing(false)}
      onKeyDown={capturing ? onKeyDown : undefined}
      aria-label={capturing ? "Press the new shortcut" : `Shortcut: ${displayHotkey(hotkey)}. Click to change.`}
    >
      {capturing ? "Press keys…" : displayHotkey(hotkey)}
    </Button>
  );
}
