import {
  currentEnvironmentIdentificationMode,
  type EnvironmentIdentificationMode,
} from "@onicode/contracts/settings";
import { useEffect, useRef } from "react";

import { runSky } from "./sidebarSky/skyLoop";

/** Whether the sidebar header shows its scenery. "dev-artwork" is the name older settings saved. */
export function showsSidebarStageBackdrop(mode: EnvironmentIdentificationMode): boolean {
  return currentEnvironmentIdentificationMode(mode) === "artwork";
}

/**
 * A landscape behind the sidebar header that follows the local time of day. Its nearest ridge is
 * cut out of the picture, so the sidebar's own surface is the ground.
 */
export function SidebarStageBackdrop() {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- paints a canvas on a timer and stops while it is hidden
  useEffect(() => {
    const canvas = canvasRef.current;
    // The sidebar toggle floats outside the header, so the colours go on the wrapper they share.
    // A narrow window's sidebar is a sheet outside that wrapper, with its toggle in the header.
    const controlsHost =
      canvas?.closest<HTMLElement>('[data-slot="sidebar-wrapper"]') ?? canvas?.parentElement;
    return canvas && controlsHost ? runSky(canvas, controlsHost) : undefined;
  }, []);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden="true"
      className="pointer-events-none absolute inset-x-0 top-0 z-0 h-26 w-full select-none"
    />
  );
}
