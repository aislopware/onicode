import { RefreshIcon } from "~/components/ui/refresh-icon";
import { CheckIcon, CircleArrowUpIcon, DownloadIcon, RotateCwIcon } from "lucide-react";
import type { AnimationEventHandler, CSSProperties, ReactNode } from "react";

const DOWNLOAD_PROGRESS_RADIUS = 14;
const DOWNLOAD_PROGRESS_CIRCUMFERENCE = 2 * Math.PI * DOWNLOAD_PROGRESS_RADIUS;

export type DesktopUpdateStatusIconState =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "downloaded"
  | "everything";

function normalizeDesktopUpdateDownloadPercent(percent: number | null): number {
  if (percent === null || !Number.isFinite(percent)) return 0;
  return Math.min(100, Math.max(0, percent));
}

export function shouldShowDesktopUpdateCheckIcon({
  isAnimationLatched,
  isChecking,
  prefersReducedMotion,
}: {
  readonly isAnimationLatched: boolean;
  readonly isChecking: boolean;
  readonly prefersReducedMotion: boolean;
}): boolean {
  return isChecking || (isAnimationLatched && !prefersReducedMotion);
}

export function shouldContinueDesktopUpdateCheckAnimation({
  isChecking,
  prefersReducedMotion,
}: {
  readonly isChecking: boolean;
  readonly prefersReducedMotion: boolean;
}): boolean {
  return isChecking && !prefersReducedMotion;
}

function DesktopUpdateAvailableIcon() {
  return (
    <span className="relative grid size-4 place-items-center">
      <DownloadIcon className="size-4" />
      <span
        aria-hidden="true"
        className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-current ring-2 ring-sidebar-control-surface"
      />
    </span>
  );
}

/**
 * A 32px ring around a 16px glyph. `drainMs` empties the ring from `fraction` over that long,
 * for a countdown; without it the arc follows `fraction`.
 */
export function UpdateProgressRing({
  fraction,
  drainMs,
  children,
}: {
  readonly fraction: number;
  readonly drainMs?: number | undefined;
  readonly children: ReactNode;
}) {
  const offset = DOWNLOAD_PROGRESS_CIRCUMFERENCE * (1 - Math.min(1, Math.max(0, fraction)));

  return (
    <span className="relative grid size-8 place-items-center">
      <svg
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 size-full -rotate-90"
        viewBox="0 0 32 32"
      >
        <circle
          cx="16"
          cy="16"
          r={DOWNLOAD_PROGRESS_RADIUS}
          fill="none"
          className="stroke-current/22"
          strokeWidth="1.5"
        />
        <circle
          cx="16"
          cy="16"
          r={DOWNLOAD_PROGRESS_RADIUS}
          fill="none"
          stroke="currentColor"
          strokeDasharray={DOWNLOAD_PROGRESS_CIRCUMFERENCE}
          strokeLinecap="round"
          strokeWidth="1.5"
          className={
            drainMs === undefined
              ? "[stroke-dashoffset:var(--ring-offset)] transition-[stroke-dashoffset] duration-300 ease-linear motion-reduce:transition-none"
              : "[stroke-dashoffset:var(--ring-empty)] transition-[stroke-dashoffset] duration-(--ring-drain-ms) ease-linear starting:[stroke-dashoffset:var(--ring-offset)]"
          }
          style={
            {
              "--ring-offset": offset,
              "--ring-empty": DOWNLOAD_PROGRESS_CIRCUMFERENCE,
              "--ring-drain-ms": `${drainMs ?? 0}ms`,
              // A round cap paints a dot at zero length.
              opacity: fraction > 0 ? 1 : 0,
            } as CSSProperties
          }
        />
      </svg>
      {children}
    </span>
  );
}

function DesktopUpdateDownloadingIcon({ percent }: { readonly percent: number | null }) {
  return (
    <UpdateProgressRing fraction={normalizeDesktopUpdateDownloadPercent(percent) / 100}>
      <DownloadIcon className="size-4" />
    </UpdateProgressRing>
  );
}

function DesktopUpdateDownloadedIcon() {
  return (
    <span className="relative grid size-4 place-items-center">
      <RotateCwIcon className="size-4" />
      <span className="absolute -right-1 -bottom-1 grid size-2.5 place-items-center rounded-full bg-foreground text-background ring-2 ring-background">
        <CheckIcon className="size-2" strokeWidth={3} />
      </span>
    </span>
  );
}

export function DesktopUpdateStatusIcon({
  downloadPercent,
  isCheckAnimating,
  onCheckAnimationIteration,
  status,
}: {
  readonly downloadPercent?: number | null;
  readonly isCheckAnimating?: boolean;
  readonly onCheckAnimationIteration?: AnimationEventHandler<SVGSVGElement>;
  readonly status: DesktopUpdateStatusIconState;
}) {
  if (status === "available") return <DesktopUpdateAvailableIcon />;
  if (status === "downloading") {
    return <DesktopUpdateDownloadingIcon percent={downloadPercent ?? null} />;
  }
  if (status === "downloaded") return <DesktopUpdateDownloadedIcon />;
  if (status === "everything") return <CircleArrowUpIcon className="size-4" />;

  return (
    <RefreshIcon
      className="size-4"
      refreshing={status === "checking" && isCheckAnimating === true}
      onAnimationIteration={onCheckAnimationIteration}
    />
  );
}
