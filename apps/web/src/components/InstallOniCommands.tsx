import { cn } from "~/lib/utils";

import { CommandBlock } from "./CommandBlock";

/** Setup steps for another computer: install `onicode`, then run `command`. */
export function InstallOniCommands({
  command,
  className,
}: {
  readonly command: string;
  readonly className?: string;
}) {
  return (
    <div className={cn("space-y-2", className)}>
      <CommandBlock command="npm install -g onicode" />
      <p className="text-xs text-muted-foreground">Then</p>
      <CommandBlock command={command} />
      <p className="text-xs text-muted-foreground">
        Skip the install if you already have <code className="font-mono">onicode</code>. If{" "}
        <code className="font-mono">onicode</code> is not found after installing, add npm's global
        bin folder to your PATH.
      </p>
    </div>
  );
}
