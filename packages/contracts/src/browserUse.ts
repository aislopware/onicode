import * as Schema from "effect/Schema";

import { ComputerDriverInstall } from "./computer.ts";

export const BrowserUseStatus = Schema.Struct({
  driverVersion: Schema.String,
  /** obu installs the way cua-driver does, so it reports the same states. */
  install: ComputerDriverInstall,
});
export type BrowserUseStatus = typeof BrowserUseStatus.Type;
