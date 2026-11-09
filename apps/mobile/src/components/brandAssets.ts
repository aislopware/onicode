import Constants from "expo-constants";

export const ONICODE_CODE_BRAND_MARK_SOURCE =
  Constants.expoConfig?.extra?.appVariant === "development"
    ? require("../../../../assets/dev/blueprint-ios-1024.png")
    : require("../../../../assets/prod/black-ios-1024.png");
