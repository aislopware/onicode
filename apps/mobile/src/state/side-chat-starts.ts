import { msg } from "gt-react-native";

import * as Dialog from "../components/ui/Dialog";
import { m } from "../i18n/messages";

// Module-wide, so a thread screen mounted again sees a start the one before it began.
const startingThreadKeys = new Set<string>();

export function beginSideChatStart(threadKey: string): () => void {
  startingThreadKeys.add(threadKey);
  return () => startingThreadKeys.delete(threadKey);
}

/** The draft holds the side question until its side chat starts, so a send now would resend it. */
export function alertIfSideChatStarting(threadKey: string): boolean {
  if (!startingThreadKeys.has(threadKey)) return false;
  Dialog.alert(
    m(msg("Still sending your previous message.")),
    m(msg("Send again once it goes through or fails.")),
  );
  return true;
}
