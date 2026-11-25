import { Circle } from "lucide-react";
import { Button } from "@/components/ui/button";

export function Meetings() {
  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col p-8">
      <h1 className="text-lg font-semibold">Meetings</h1>
      <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
        <p className="text-sm text-muted-foreground">No meetings yet.</p>
        {/* Recording is implemented in M1. */}
        <Button size="lg" disabled>
          <Circle className="fill-current text-red-500" aria-hidden />
          Record
        </Button>
      </div>
    </div>
  );
}
