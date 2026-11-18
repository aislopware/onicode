import Constants from "expo-constants";
import { Image } from "expo-image";
import { msg, T, useMessages } from "gt-react-native";
import { View } from "react-native";

import { AppText as Text } from "./ui/AppText";
import { ONICODE_CODE_BRAND_MARK_SOURCE } from "./brandAssets";
import { resolveMobileStageLabel } from "../lib/mobileBranding";

const STAGE_LABEL =
  resolveMobileStageLabel(Constants.expoConfig?.extra?.appVariant) === null
    ? null
    : msg("Dev", { $context: "app release stage badge" });

export function BrandMark(props: { readonly compact?: boolean }) {
  const m = useMessages();
  const compact = props.compact ?? false;
  const iconSize = compact ? 32 : 44;
  const stageLabel = STAGE_LABEL === null ? null : m(STAGE_LABEL);

  return (
    <View className="flex-row items-center gap-3">
      <Image
        source={ONICODE_CODE_BRAND_MARK_SOURCE}
        accessibilityIgnoresInvertColors
        style={{
          width: iconSize,
          height: iconSize,
          borderRadius: compact ? 10 : 14,
        }}
      />
      <View className="gap-1">
        <View className="flex-row items-center gap-2">
          <Text className="text-lg font-oni-bold tracking-[-0.4px] text-foreground">Onicode</Text>
          {stageLabel !== null ? (
            <View className="rounded-full bg-subtle px-2 py-1">
              <Text className="text-3xs font-oni-bold tracking-[1.1px] uppercase text-foreground-muted">
                {stageLabel}
              </Text>
            </View>
          ) : null}
        </View>
        {!compact ? (
          <Text className="text-xs font-medium text-foreground-muted">
            <T>Mobile control surface for your live coding environments</T>
          </Text>
        ) : null}
      </View>
    </View>
  );
}
