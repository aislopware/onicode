import type { AuthMcpClientAccess } from "@onicode/contracts";
import { EyeIcon, type LucideIcon } from "lucide-react";

import { runtimeModeConfig } from "../chat/runtimeModeConfig";

export const accessConfig: Record<
  AuthMcpClientAccess,
  { readonly label: string; readonly description: string; readonly icon: LucideIcon }
> = {
  "read-only": {
    label: "Read only",
    description: "Read projects and threads. Cannot start, message or change anything.",
    icon: EyeIcon,
  },
  ...runtimeModeConfig,
};
