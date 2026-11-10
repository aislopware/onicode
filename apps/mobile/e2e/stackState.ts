import * as NodePath from "node:path";

/** Onicode Dev, the development client the suite drives. */
export const BUNDLE_ID = "dev.aislopware.onicode.dev";

/** Metro's port for the device suite, clear of the default 8081 a developer's own Metro holds. */
export const METRO_PORT = 8090;

/** Where stack.ts tells the tests how to reach the server it started. */
export const STACK_STATE_FILE = NodePath.resolve(import.meta.dirname, "../.e2e/stack.json");

export interface StackState {
  readonly baseDir: string;
  readonly projectDir: string;
  readonly serverPort: number;
  readonly environmentId: string;
}
