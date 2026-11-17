import { useEffect, useState } from "react";

import { getDocumentActivity, subscribeDocumentActivity } from "~/lib/documentActivity";
import { isMacPlatform } from "~/lib/utils";
import { readLocalApi } from "~/localApi";
import { isLocalEnvironmentDisabled } from "~/localEnvironment";
import { Button } from "../ui/button";
import { toastManager } from "../ui/toast";
import { SettingsRow } from "./settingsLayout";
import { searchableSetting } from "./settingsSearch";

const NO_PANE_HINT =
  "If Privacy & Security has no Developer Tools entry, run `spctl developer-mode enable-terminal` in Terminal first.";

/**
 * Whether macOS lists Onicode under Developer Tools. Only agents this desktop app runs inherit
 * the grant, so it is offered only while the app hosts this computer's environment.
 */
export function useDeveloperToolsAccess() {
  const supported =
    window.desktopBridge !== undefined &&
    isMacPlatform(navigator.platform) &&
    !isLocalEnvironmentDisabled();
  const [granted, setGranted] = useState<boolean | null>(null);

  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- the grant lives in System Settings, so check again when the window regains focus
  useEffect(() => {
    const bridge = window.desktopBridge;
    if (!supported || !bridge) return;
    const requests = { latest: 0 };
    const check = () => {
      requests.latest += 1;
      const request = requests.latest;
      void bridge.checkSystemPermission("developer-tools").then(
        (next) => {
          if (request === requests.latest) setGranted(next);
        },
        () => {
          if (request === requests.latest) setGranted(null);
        },
      );
    };
    check();
    let previous = getDocumentActivity();
    const stop = subscribeDocumentActivity(() => {
      const next = getDocumentActivity();
      if (next.focused && !previous.focused) check();
      previous = next;
    });
    return () => {
      requests.latest = -1;
      stop();
    };
  }, [supported]);

  const openSettings = () =>
    void readLocalApi()
      ?.shell.openSystemSettings("developer-tools")
      .catch(() => {
        toastManager.add({
          type: "error",
          title: "Could not open System Settings",
          description: `Open Privacy & Security → Developer Tools manually. ${NO_PANE_HINT}`,
        });
      });

  return { granted: supported ? granted : null, openSettings };
}

export function DeveloperToolsSettingsRow() {
  const { granted, openSettings } = useDeveloperToolsAccess();
  if (granted === null) return null;
  return (
    <SettingsRow
      {...searchableSetting("developer-tools")}
      description={
        granted
          ? "Programs that agents on this Mac build or download start without waiting for macOS security scans."
          : `macOS scans each new program that agents build or download, which can use a lot of CPU. Add Onicode to Developer Tools to skip those scans. ${NO_PANE_HINT}`
      }
      control={
        <Button size="sm" variant="outline" onClick={openSettings}>
          {granted ? "Open Settings" : "Allow"}
        </Button>
      }
    />
  );
}
