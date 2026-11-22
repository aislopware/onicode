import { useGT } from "gt-react-native";
import { View } from "react-native";

import { AppActivityIndicator } from "../../components/ui/AppActivityIndicator";
import { AppText as Text } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";

/** Shown while a send waits for pasted images to finish converting. */
export function ComposerHeldSendBanner(props: {
  readonly images: number;
  readonly onCancel: () => void;
}) {
  const gt = useGT();
  return (
    <View className="flex-row items-center gap-2 px-4 py-1.5">
      <AppActivityIndicator size="small" colorClassName="accent-foreground-muted" />
      <Text
        accessibilityLiveRegion="polite"
        className="min-w-0 flex-1 text-xs text-foreground-muted"
        numberOfLines={1}
      >
        {props.images > 0
          ? gt(
              "{count, plural, one {Sending once image is ready} other {Sending once {count} images are ready}}",
              { count: props.images },
            )
          : gt("Sending message")}
      </Text>
      <Button label={gt("Cancel")} tone="text" size="compact" onPress={props.onCancel} />
    </View>
  );
}
