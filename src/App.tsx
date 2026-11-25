import { useState } from "react";
import { AudioLines, Settings as SettingsIcon, Users } from "lucide-react";
import { cn } from "cn";
import { UpdateBanner } from "@/components/UpdateBanner";
import { MeetingView } from "@/screens/MeetingView";
import { Meetings } from "@/screens/Meetings";
import { Speakers } from "@/screens/Speakers";
import { Settings } from "@/screens/Settings";

type Screen = "meetings" | "speakers" | "settings";

const NAV: { id: Screen; label: string; icon: typeof Users }[] = [
  { id: "meetings", label: "Meetings", icon: AudioLines },
  { id: "speakers", label: "Speakers", icon: Users },
  { id: "settings", label: "Settings", icon: SettingsIcon },
];

export default function App() {
  const [screen, setScreen] = useState<Screen>("meetings");
  const [openMeeting, setOpenMeeting] = useState<number | null>(null);
  const [settingsTab, setSettingsTab] = useState("general");
  const go = (s: Screen) => {
    setScreen(s);
    setOpenMeeting(null);
    if (s === "settings") setSettingsTab("general");
  };

  return (
    <div className="flex h-screen bg-background text-foreground select-none">
      <nav aria-label="Main" className="flex w-52 shrink-0 flex-col gap-1 border-r border-border bg-sidebar p-3">
        <div className="px-2 pt-1 pb-4 text-sm font-semibold tracking-tight">Earshot</div>
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
        <main className="flex-1 overflow-y-auto">
          {screen === "meetings" && openMeeting === null && (
            <Meetings
              onOpen={setOpenMeeting}
              onSetupTranscription={() => {
                setScreen("settings");
                setSettingsTab("transcription");
              }}
            />
          )}
          {screen === "meetings" && openMeeting !== null && (
            <MeetingView id={openMeeting} onBack={() => setOpenMeeting(null)} />
          )}
          {screen === "speakers" && <Speakers />}
          {screen === "settings" && <Settings tab={settingsTab} onTab={setSettingsTab} />}
        </main>
        <UpdateBanner />
      </div>
    </div>
  );
}
