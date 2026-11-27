import { lazy, Suspense, useEffect, useState } from "react";
import { AudioLines, Settings as SettingsIcon, Users } from "lucide-react";
import { cn } from "cn";
import icon from "@/assets/icon.png";
import { ShortcutsDialog } from "@/components/ShortcutsDialog";
import { UpdateBanner } from "@/components/UpdateBanner";
import { FOCUS_SEARCH } from "@/lib/events";
import { MeetingView } from "@/screens/MeetingView";
import { Meetings } from "@/screens/Meetings";
// Settings and Speakers load when first opened, keeping the startup bundle small.
const Settings = lazy(() => import("@/screens/Settings").then((m) => ({ default: m.Settings })));
const Speakers = lazy(() => import("@/screens/Speakers").then((m) => ({ default: m.Speakers })));

type Screen = "meetings" | "speakers" | "settings";

const NAV: { id: Screen; label: string; icon: typeof Users }[] = [
  { id: "meetings", label: "Meetings", icon: AudioLines },
  { id: "speakers", label: "Speakers", icon: Users },
  { id: "settings", label: "Settings", icon: SettingsIcon },
];

export default function App() {
  const [screen, setScreen] = useState<Screen>("meetings");
  const [openMeeting, setOpenMeeting] = useState<number | null>(null);
  // Where to start playback when a meeting is opened from a search result.
  const [openAt, setOpenAt] = useState<number | undefined>(undefined);
  const [settingsTab, setSettingsTab] = useState("general");
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const go = (s: Screen) => {
    setScreen(s);
    setOpenMeeting(null);
    if (s === "settings") setSettingsTab("general");
  };

  // App-wide shortcuts (the recording hotkey is a global OS shortcut, handled natively).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return;
      const nav: Record<string, Screen> = { "1": "meetings", "2": "speakers", ",": "settings" };
      if (nav[e.key]) {
        e.preventDefault();
        go(nav[e.key]);
      } else if (e.key === "/") {
        e.preventDefault();
        setShortcutsOpen((o) => !o);
      } else if (e.key.toLowerCase() === "f") {
        e.preventDefault();
        go("meetings");
        // After the list has rendered.
        setTimeout(() => window.dispatchEvent(new Event(FOCUS_SEARCH)), 0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="flex h-screen bg-background text-foreground select-none">
      <nav aria-label="Main" className="flex w-52 shrink-0 flex-col gap-1 border-r border-border bg-sidebar p-3">
        <div className="flex items-center gap-2 px-2 pt-1 pb-4 text-sm font-semibold tracking-tight">
          <img src={icon} alt="" className="size-6 rounded-md dark:ring-1 dark:ring-white/10" />
          Earshot
        </div>
        {NAV.map(({ id, label, icon: Icon }) => (
          <button
            key={id}
            type="button"
            onClick={() => go(id)}
            aria-current={screen === id ? "page" : undefined}
            className={cn(
              "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-sidebar-foreground outline-none",
              "hover:bg-sidebar-accent focus-visible:ring-2 focus-visible:ring-ring",
              screen === id && "bg-sidebar-accent font-medium text-sidebar-accent-foreground",
            )}
          >
            <Icon className="size-4" aria-hidden />
            {label}
          </button>
        ))}
      </nav>
      <div className="flex min-w-0 flex-1 flex-col">
        <main className="flex-1 overflow-y-auto [scrollbar-gutter:stable]">
          <Suspense fallback={null}>
          {screen === "meetings" && openMeeting === null && (
            <Meetings
              onOpen={(id, atMs) => {
                setOpenAt(atMs);
                setOpenMeeting(id);
              }}
              onOpenSettings={(tab) => {
                setScreen("settings");
                setSettingsTab(tab);
              }}
            />
          )}
          {screen === "meetings" && openMeeting !== null && (
            <MeetingView
              id={openMeeting}
              startAtMs={openAt}
              onBack={() => setOpenMeeting(null)}
              onOpenSettings={(tab) => {
                setScreen("settings");
                setSettingsTab(tab);
              }}
            />
          )}
          {screen === "speakers" && (
            <Speakers
              onOpenMeeting={(id) => {
                setScreen("meetings");
                setOpenAt(undefined);
                setOpenMeeting(id);
              }}
            />
          )}
          {screen === "settings" && <Settings tab={settingsTab} onTab={setSettingsTab} onShowShortcuts={() => setShortcutsOpen(true)} />}
          </Suspense>
        </main>
        <UpdateBanner />
      </div>
      <ShortcutsDialog open={shortcutsOpen} onOpenChange={setShortcutsOpen} />
    </div>
  );
}
