import { useAtomValue } from "@effect/atom-react";
import { Atom } from "effect/reactivity";
import { useLocale, useMessages } from "gt-react-native";
import * as Linking from "expo-linking";
import { useEffect } from "react";
import { Platform } from "react-native";
import { useAppearancePreferences } from "../features/settings/appearance/AppearancePreferencesProvider";
import { useSystem24HourClock } from "../features/threads/useMessageTimeFormatter";
import { environmentCatalog } from "../connection/catalog";
import { environmentPresentations } from "../state/presentation";
import { publishSubscriptionUsage } from "./publishSubscriptionUsage";
import { buildSubscriptionUsageSnapshot } from "./subscriptionUsageSnapshot";
import { createWidgetTheme } from "./widgetTheme";
import { useSettledLocale } from "../i18n/I18nProvider";
import type { MessageResolver } from "../i18n/messages";

// Isolate quota changes from the much busier thread/config presentation stream.
// One atom per UI language and clock, so switching either republishes the widget's copy.
const snapshotAtoms = new Map<string, ReturnType<typeof makeSnapshotAtom>>();

function makeSnapshotAtom(resolve: MessageResolver, is24Hour: boolean | undefined) {
  return Atom.make((get) =>
    buildSubscriptionUsageSnapshot(
      get(environmentPresentations.presentationsWithProvidersAtom),
      Linking.createURL("settings/usage", { queryParams: { tab: "limits" } }),
      Platform.OS === "android" ? Infinity : 6,
      resolve,
      is24Hour,
    ),
  ).pipe(Atom.withEquality((a, b) => JSON.stringify(a) === JSON.stringify(b)));
}

function snapshotAtom(locale: string, resolve: MessageResolver, is24Hour: boolean | undefined) {
  const key = `${locale}:${is24Hour}`;
  let atom = snapshotAtoms.get(key);
  if (atom === undefined) {
    atom = makeSnapshotAtom(resolve, is24Hour);
    snapshotAtoms.set(key, atom);
  }
  return atom;
}

export function SubscriptionUsageCoordinator() {
  const {
    themeMode,
    themeAppearance,
    themeVariablesByAppearance,
    isReady: appearanceReady,
  } = useAppearancePreferences();
  const catalog = useAtomValue(environmentCatalog.catalogValueAtom);
  const resolve = useMessages();
  const locale = useLocale();
  const languageReady = useSettledLocale() !== null;
  const is24Hour = useSystem24HourClock();
  const snapshot = useAtomValue(snapshotAtom(locale, resolve, is24Hour));
  const androidAppearance = Platform.OS === "android" ? themeAppearance : null;
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- publishes subscription usage and theme to the native home screen widget
  useEffect(() => {
    if (!catalog.isReady || !appearanceReady || !languageReady) return;
    void Promise.resolve()
      .then(() =>
        publishSubscriptionUsage({
          ...snapshot,
          theme: createWidgetTheme(themeMode, themeVariablesByAppearance),
        }),
      )
      .catch((error: unknown) => {
        console.warn("Could not update subscription usage widget", error);
      });
  }, [
    catalog.isReady,
    appearanceReady,
    languageReady,
    snapshot,
    themeMode,
    themeVariablesByAppearance,
    // oxlint-disable-next-line react/exhaustive-effect-dependencies -- Android stores resolved colors; System appearance changes must republish them.
    androidAppearance,
  ]);
  return null;
}
