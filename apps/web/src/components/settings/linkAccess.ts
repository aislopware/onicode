import type { AuthMcpClientAccess } from "@onicode/contracts";
import { msg } from "gt-react";
import { EyeIcon, type LucideIcon } from "lucide-react";

import { runtimeModeConfig } from "../chat/runtimeModeConfig";

export const accessConfig: Record<
  AuthMcpClientAccess,
  { readonly label: string; readonly description: string; readonly icon: LucideIcon }
> = {
  "read-only": {
    label: msg("Read only"),
    description: msg("Read projects and threads. Cannot start, message or change anything."),
    icon: EyeIcon,
  },
  ...runtimeModeConfig,
};
