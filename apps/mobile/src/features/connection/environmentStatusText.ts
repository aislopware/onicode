import {
  connectionStatusText,
  type EnvironmentConnectionPresentation,
} from "@onicode/client-runtime/connection";

export function environmentStatusText(connection: EnvironmentConnectionPresentation): string {
  return connection.phase === "unsupported" && connection.error !== null
    ? connection.error
    : connectionStatusText(connection);
}
