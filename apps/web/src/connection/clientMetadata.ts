import type { AuthClientPresentationMetadata, ClientOs, DesktopBridge } from "@onicode/contracts";

function clientOsFromElectronPlatform(platform: string | undefined): ClientOs {
  switch (platform) {
    case "darwin":
      return "macOS";
    case "win32":
      return "Windows";
    case "linux":
      return "Linux";
    default:
      return platform ? "other" : "unknown";
  }
}

export function clientPresentationMetadata(input: {
  readonly appVersion: string;
  readonly desktopBridge: Pick<DesktopBridge, "getClientPlatform"> | undefined;
}): AuthClientPresentationMetadata {
  return {
    label: "Onicode Desktop",
    deviceType: "desktop",
    os: clientOsFromElectronPlatform(input.desktopBridge?.getClientPlatform()),
    surface: "desktop",
    ...(input.appVersion === "0.0.0" ? {} : { appVersion: input.appVersion }),
  };
}
