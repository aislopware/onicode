import { useAtomValue } from "@effect/atom-react";
import { useNavigation } from "@react-navigation/native";
import { AsyncResult } from "effect/reactivity";
import { Image } from "expo-image";
import * as Haptics from "expo-haptics";
import { T, useGT } from "gt-react-native";
import { useEffect, useState } from "react";
import { View } from "react-native";

import { AppText as Text, AppTextInput as TextInput } from "../../components/ui/AppText";
import { Button } from "../../components/ui/Button";
import { FitSheet } from "../../components/ui/FitSheet";
import { ListToggleRow } from "../../components/ui/ListToggleRow";
import { SheetHeader } from "../../components/ui/SheetHeader";
import { sendBugReport } from "../../lib/crash-reporting";
import { openSettings } from "../settings/settings-navigation";
import { mobilePreferencesAtom } from "../../state/preferences";
import {
  bugReportAvailability,
  currentBugReportDraft,
  releaseBugReportDraft,
  screenshotUri,
} from "./bug-report";

/** The bug report form; opened by a shake, Diagnostics, and error screens. */
export function BugReportSheet() {
  const gt = useGT();
  const navigation = useNavigation();
  const preferences = useAtomValue(mobilePreferencesAtom);
  const savedEnabled = AsyncResult.isSuccess(preferences)
    ? preferences.value.errorReportingEnabled !== false
    : null;
  const availability = bugReportAvailability(savedEnabled);
  const [draft] = useState(currentBugReportDraft);
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- frees the module-held screenshot when the sheet goes
  useEffect(() => () => releaseBugReportDraft(draft), [draft]);
  // Sent as the sheet opens, so it shows what led up to the report rather than this sheet.
  const [replayId] = useState(() => (availability === "ready" ? draft.sendReplay() : undefined));
  const [screenshotPreview] = useState(() =>
    draft.screenshot ? screenshotUri(draft.screenshot) : undefined,
  );
  const [message, setMessage] = useState("");
  const [email, setEmail] = useState("");
  // Sentry masks text and images in the native capture, so it can go by default.
  const [attachScreenshot, setAttachScreenshot] = useState(true);
  const [status, setStatus] = useState<"editing" | "sending" | "failed" | "sent">("editing");
  const close = () => navigation.goBack();
  const header = <SheetHeader title={gt("Report a bug")} onCancel={close} />;

  if (availability !== "ready") {
    return (
      <FitSheet header={header} contentContainerClassName="px-5 pt-4 gap-5">
        <Text className="text-base text-foreground">
          {availability === "restart"
            ? gt("Restart Onicode to send bug reports.")
            : availability === "off"
              ? gt(
                  "Bug reports travel with error reports, which are off on this device. Turn them on in Diagnostics, then restart Onicode.",
                )
              : gt("This build of Onicode does not send bug reports.")}
        </Text>
        {availability === "off" ? (
          <Button
            label={gt("Open Diagnostics")}
            fullWidth
            onPress={() => openSettings(navigation, "SettingsDiagnostics")}
          />
        ) : null}
      </FitSheet>
    );
  }

  if (status === "sent") {
    return (
      <FitSheet header={header} contentContainerClassName="px-5 pt-4 gap-5">
        <Text className="text-base text-foreground">
          <T>Bug report sent. Thank you.</T>
        </Text>
        <Button label={gt("Done")} tone="primary" fullWidth onPress={close} />
      </FitSheet>
    );
  }

  const trimmed = message.trim();
  const submit = async () => {
    if (trimmed.length === 0 || status === "sending") return;
    setStatus("sending");
    const sent = await sendBugReport({
      message: trimmed,
      email: email.trim() || undefined,
      associatedEventId: draft.associatedEventId,
      screenshot: attachScreenshot ? draft.screenshot : undefined,
      replayId: replayId ?? draft.sendReplay(),
    });
    if (!sent) {
      setStatus("failed");
      return;
    }
    void Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success).catch(() => {});
    setStatus("sent");
  };

  return (
    <FitSheet
      header={header}
      avoidsKeyboard
      keyboardShouldPersistTaps="handled"
      keyboardDismissMode="on-drag"
      automaticallyAdjustKeyboardInsets
      contentContainerClassName="px-5 pt-4 gap-5"
      footer={
        <Button
          label={gt("Send report")}
          tone="primary"
          fullWidth
          disabled={trimmed.length === 0}
          loading={status === "sending"}
          onPress={() => void submit()}
        />
      }
    >
      <Text className="text-sm text-foreground-muted">
        <T>
          Your report goes to the publisher of this Onicode build with a short screen replay from
          before you opened this report, recent errors and app details. Replays mask all text and
          media.
        </T>
      </Text>
      <View className="gap-2">
        <Text className="text-sm font-oni-medium text-foreground">
          <T>What happened?</T>
        </Text>
        <TextInput
          multiline
          autoFocus
          accessibilityLabel={gt("What happened?")}
          value={message}
          onChangeText={setMessage}
          placeholder={gt("What you did, what you expected, and what happened instead")}
          textAlignVertical="top"
          className="min-h-28 rounded-xl bg-subtle px-4 py-3 text-base"
        />
      </View>
      <View className="gap-2">
        <Text className="text-sm font-oni-medium text-foreground">
          <T>Email (optional)</T>
        </Text>
        <TextInput
          accessibilityLabel={gt("Email (optional)")}
          value={email}
          onChangeText={setEmail}
          placeholder={gt("Only to reply about this report")}
          keyboardType="email-address"
          autoCapitalize="none"
          autoCorrect={false}
          textContentType="emailAddress"
          className="rounded-xl bg-subtle px-4 py-3 text-base"
        />
      </View>
      {screenshotPreview ? (
        <View className="gap-3">
          <ListToggleRow
            icon="camera"
            label={gt("Attach a screenshot")}
            description={gt("Text and images in it are masked.")}
            value={attachScreenshot}
            onValueChange={setAttachScreenshot}
          />
          <Image
            source={{ uri: screenshotPreview }}
            accessibilityLabel={gt("Screenshot of the screen")}
            contentFit="contain"
            style={{ width: "100%", height: 224, borderRadius: 12 }}
          />
        </View>
      ) : null}
      {status === "failed" ? (
        <Text accessibilityRole="alert" className="text-sm text-danger-foreground">
          <T>The report could not be sent. Try again.</T>
        </Text>
      ) : null}
    </FitSheet>
  );
}
