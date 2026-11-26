import { useEffect, useState } from "react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { recorder } from "@/lib/recorder";

const mac = typeof navigator !== "undefined" && navigator.userAgent.includes("Mac");
export const MOD = mac ? "⌘" : "Ctrl";

/** Human form of an accelerator such as "CommandOrControl+Shift+R". */
export function formatAccelerator(acc: string) {
  return acc
    .replace("CommandOrControl", MOD)
    .replace("Shift", mac ? "⇧" : "Shift")
    .replace("Alt", mac ? "⌥" : "Alt")
    .split("+");
}

function Keys({ keys }: { keys: string[] }) {
  return (
    <span className="flex gap-1">
      {keys.map((k) => (
        <kbd key={k} className="min-w-6 rounded border border-border bg-muted px-1.5 py-0.5 text-center font-sans text-xs">
          {k}
        </kbd>
      ))}
    </span>
  );
}

export function ShortcutsDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const [hotkey, setHotkey] = useState("");
  useEffect(() => {
    if (open) recorder.getHotkey().then(setHotkey).catch(() => {});
  }, [open]);

  const groups: { title: string; items: [string, string[]][] }[] = [
    {
      title: "Anywhere",
      items: [
        ["Start or stop recording (even when Earshot is in the background)", hotkey ? formatAccelerator(hotkey) : ["—"]],
        ["Search meetings", [MOD, "F"]],
        ["Meetings", [MOD, "1"]],
        ["Speakers", [MOD, "2"]],
        ["Settings", [MOD, ","]],
        ["Keyboard shortcuts", [MOD, "/"]],
      ],
    },
    {
      title: "In a meeting",
      items: [
        ["Play or pause", ["Space"]],
        ["Back or forward 5 seconds", ["←", "→"]],
        ["Back or forward 10 seconds", ["Shift", "← →"]],
        ["Back to all meetings", ["Esc"]],
      ],
    },
  ];

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>Change the recording shortcut in Settings → Recording.</DialogDescription>
        </DialogHeader>
        {groups.map((g) => (
          <section key={g.title} aria-label={g.title}>
            <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">{g.title}</h3>
            <dl className="mt-2 space-y-2">
              {g.items.map(([label, keys]) => (
                <div key={label} className="flex items-center justify-between gap-4 text-sm">
                  <dt>{label}</dt>
                  <dd>
                    <Keys keys={keys} />
                  </dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </DialogContent>
    </Dialog>
  );
}
