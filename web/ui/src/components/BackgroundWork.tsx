import { useState } from "react";
import { BotIcon, ChevronDownIcon, TerminalIcon } from "lucide-react";

import type { Session } from "@/api/types";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useNow } from "@/hooks/useNow";
import { relTime } from "@/lib/format";
import { backgroundLabel } from "@/lib/sessions";
import { cn } from "@/lib/utils";

/**
 * What a session runs beside its main thread (`background`, core/background.rs): its subagents
 * and background shells, as icons and counts (the tooltip says what). Nothing when none. Not a link —
 * rows and cards are links already.
 */
export function BackgroundChip({
  session: s,
  className,
}: {
  session: Pick<Session, "background">;
  className?: string;
}) {
  const label = backgroundLabel(s.background);
  if (!label) return null;
  const agents = s.background?.agents ?? [];
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          aria-label={`Runs ${label}`}
          className={cn(
            "inline-flex h-4.5 shrink-0 items-center gap-1 rounded-[5px] border border-status-busy/40 px-1 text-[0.6875rem] text-status-busy tabular-nums",
            className,
          )}
        >
          {agents.length ? (
            <>
              <BotIcon className="size-3 shrink-0" />
              {agents.length}
            </>
          ) : null}
          {s.background?.shells ? (
            <>
              <TerminalIcon className="size-3 shrink-0" />
              {s.background.shells}
            </>
          ) : null}
        </span>
      </TooltipTrigger>
      <TooltipContent>
        Runs {label}
        {agents.length
          ? `: ${agents.map((a) => a.description || a.type || "agent").join(" · ")}`
          : ""}
      </TooltipContent>
    </Tooltip>
  );
}

/**
 * Above the composer, like Claude Code's agent panel: what runs beside the main thread — each
 * subagent (what, type and model, how long, its last step) and the background shells. Folds to
 * one line. Nothing when none.
 */
export function BackgroundPanel({
  session: s,
  desktop = false,
}: {
  session: Pick<Session, "background"> | null | undefined;
  desktop?: boolean;
}) {
  const now = useNow(15 * 1000);
  const [open, setOpen] = useState(true);
  const bg = s?.background;
  const label = backgroundLabel(bg);
  if (!bg || !label) return null;
  return (
    <section
      aria-label="Running in the background"
      className="shrink-0 px-3 px-safe"
    >
      <div
        className={cn(
          "mx-auto mb-2 rounded-xl border border-status-busy/30 bg-card/80",
          desktop ? "max-w-4xl" : "max-w-3xl",
        )}
      >
        <button
          type="button"
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-muted-foreground"
          aria-expanded={open}
          onClick={() => setOpen((o) => !o)}
        >
          <BotIcon className="size-3.5 shrink-0 text-status-busy" aria-hidden />
          <span className="min-w-0 flex-1 truncate" role="status">
            <span className="font-medium text-foreground">Running {label}</span>
            {open
              ? null
              : ` · ${bg.agents.map((a) => a.description || a.type || "agent").join(" · ")}`}
          </span>
          <ChevronDownIcon
            className={cn(
              "size-3.5 shrink-0 transition-transform",
              open ? "" : "-rotate-90",
            )}
            aria-hidden
          />
        </button>
        {open ? (
          <ul className="flex max-h-48 flex-col gap-0.5 overflow-y-auto px-1.5 pb-1.5">
            {bg.agents.map((a) => (
              <li
                key={a.id || `${a.description}-${a.started_at}`}
                className="flex min-w-0 items-start gap-2 rounded-lg px-1.5 py-1"
              >
                <span
                  className="mt-1.5 size-1.5 shrink-0 animate-pulse rounded-full bg-status-busy"
                  aria-hidden
                />
                <div className="min-w-0 flex-1">
                  <div className="flex min-w-0 items-baseline gap-2">
                    <span className="min-w-0 truncate text-sm font-medium">
                      {a.description || "Subagent"}
                    </span>
                    <span className="min-w-0 shrink truncate text-xs text-dimmer">
                      {[
                        a.background ? null : "foreground",
                        a.type,
                        a.model?.replace(/^claude-/, ""),
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                    </span>
                    <span
                      className="ml-auto shrink-0 text-xs text-dimmer tabular-nums"
                      title={
                        a.last_activity_at
                          ? `Last active ${relTime(a.last_activity_at, now)} ago`
                          : undefined
                      }
                    >
                      {a.started_at ? relTime(a.started_at, now) : ""}
                    </span>
                  </div>
                  {a.activity ? (
                    <div className="truncate font-mono text-xs text-muted-foreground">
                      {a.activity}
                    </div>
                  ) : null}
                </div>
              </li>
            ))}
            {bg.shells ? (
              <li className="flex items-center gap-2 px-1.5 py-1 text-sm text-muted-foreground">
                <TerminalIcon
                  className="size-3.5 shrink-0 text-status-busy"
                  aria-hidden
                />
                {bg.shells} background shell{bg.shells === 1 ? "" : "s"}
              </li>
            ) : null}
          </ul>
        ) : null}
      </div>
    </section>
  );
}
