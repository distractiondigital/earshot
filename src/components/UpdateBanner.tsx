import { Download, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useUpdates } from "@/lib/updates";

/** A quiet bar at the bottom of the window when an update is ready to install. */
export function UpdateBanner() {
  const { status, dismissed, install, dismiss } = useUpdates();

  if (status.kind === "available" && !dismissed) {
    return (
      <Bar>
        <span className="flex-1">Earshot {status.update.version} is available.</span>
        <Button size="sm" onClick={install}>
          <Download aria-hidden />
          Update and restart
        </Button>
        <Button size="icon-sm" variant="ghost" onClick={dismiss} aria-label="Dismiss">
          <X aria-hidden />
        </Button>
      </Bar>
    );
  }
  if (status.kind === "downloading") {
    const pct = status.total ? Math.round((status.downloaded / status.total) * 100) : null;
    return (
      <Bar>
        <span className="flex-1" role="status">
          Downloading Earshot {status.update.version}
          {pct !== null ? ` (${pct}%)` : "…"}
        </span>
      </Bar>
    );
  }
  if (status.kind === "installing") {
    return (
      <Bar>
        <span className="flex-1" role="status">
          Installing Earshot {status.update.version}. The app will restart.
        </span>
      </Bar>
    );
  }
  return null;
}

function Bar({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-3 border-t border-border bg-muted/50 px-4 py-2 text-sm">{children}</div>
  );
}
