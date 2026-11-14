import Constants from "expo-constants";

export const ONICODE_CODE_BRAND_MARK_SOURCE =
  Constants.expoConfig?.extra?.appVariant === "development"
    ? require("../../../../assets/dev/blueprint-splash-logo-180.png")
    : require("../../../../assets/prod/oni-black-splash-logo-180.png");
