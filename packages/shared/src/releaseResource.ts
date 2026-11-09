export type ReleaseChannel = "dev" | "stable";

// Effect lets code attributes beat OTEL_RESOURCE_ATTRIBUTES, so the operator value is applied here.
export const releaseResourceAttributes = (
  channel: ReleaseChannel,
  operatorAttributes: Readonly<Record<string, unknown>> = {},
) => {
  const operatorValue = (key: string) => {
    const value = operatorAttributes[key];
    return typeof value === "string" && value !== "" ? value : undefined;
  };
  return {
    "onicode.release.channel": operatorValue("onicode.release.channel") ?? channel,
    "deployment.environment.name":
      operatorValue("deployment.environment.name") ??
      (channel === "dev" ? "development" : "production"),
  };
};
