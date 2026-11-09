export const isSecretsFeatureEnabled = ["1", "true"].includes(
  import.meta.env.ONICODE_FEATURE_SECRETS?.trim().toLowerCase() ?? "",
);
