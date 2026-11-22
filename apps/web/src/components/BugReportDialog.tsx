import { Link } from "@tanstack/react-router";
import { T, useGT } from "gt-react";
import { useEffect, useState } from "react";

import { closeBugReport, openBugReport, useBugReportStore } from "../bugReportStore";
import {
  bugReportAvailability,
  sendBugReport,
  type BugReportAvailability,
} from "../observability/crashReportingStartup";
import { Button } from "./ui/button";
import { Checkbox } from "./ui/checkbox";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import { Spinner } from "./ui/spinner";
import { Textarea } from "./ui/textarea";
import { toastManager } from "./ui/toast";
import { registerMenuAction } from "../shortcuts/dispatcher";

/** The bug report form; opened from Help, the command palette, Diagnostics and error screens. */
export function BugReportDialogHost() {
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- subscribes to the desktop Help menu
  useEffect(() => registerMenuAction("report-bug", () => void openBugReport()), []);
  const open = useBugReportStore((state) => state.open);
  const session = useBugReportStore((state) => state.session);
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) closeBugReport(false);
      }}
    >
      <DialogPopup>
        {/* Keyed by session so each report starts empty. */}
        <BugReportForm key={session} availability={open ? bugReportAvailability() : "ready"} />
      </DialogPopup>
    </Dialog>
  );
}

function BugReportForm({ availability }: { readonly availability: BugReportAvailability }) {
  const gt = useGT();
  const associatedEventId = useBugReportStore((state) => state.associatedEventId);
  const screenshot = useBugReportStore((state) => state.screenshot);
  const screenshotUrl = useBugReportStore((state) => state.screenshotUrl);
  const [message, setMessage] = useState("");
  const [email, setEmail] = useState("");
  const [attachScreenshot, setAttachScreenshot] = useState(false);
  const [status, setStatus] = useState<"idle" | "sending" | "failed">("idle");

  if (availability !== "ready") {
    return (
      <>
        <DialogHeader>
          <DialogTitle>
            <T>Report a bug</T>
          </DialogTitle>
          <DialogDescription>
            {availability === "restart"
              ? gt("Restart Onicode to send bug reports.")
              : availability === "off"
                ? gt(
                    "Bug reports travel with error reports, which are off for this app. Turn them on in Diagnostics, then restart Onicode.",
                  )
                : gt("This build of Onicode does not send bug reports.")}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter variant="bare">
          {availability === "off" ? (
            <Button
              size="sm"
              variant="outline"
              render={<Link to="/settings/diagnostics" />}
              onClick={() => closeBugReport(false)}
            >
              <T>Open Diagnostics</T>
            </Button>
          ) : null}
          <Button size="sm" onClick={() => closeBugReport(false)}>
            <T>Close</T>
          </Button>
        </DialogFooter>
      </>
    );
  }

  const trimmed = message.trim();
  const submit = async () => {
    if (trimmed.length === 0 || status === "sending") return;
    setStatus("sending");
    try {
      await sendBugReport({
        message: trimmed,
        email: email.trim() || undefined,
        associatedEventId,
        screenshot: attachScreenshot ? screenshot : undefined,
      });
    } catch {
      setStatus("failed");
      return;
    }
    closeBugReport(true);
    toastManager.add({ type: "success", title: gt("Bug report sent. Thank you.") });
  };

  return (
    <form
      className="contents"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <DialogHeader>
        <DialogTitle>
          <T>Report a bug</T>
        </DialogTitle>
        <DialogDescription>
          <T>
            Your report goes to the publisher of this Onicode build with the last minute of screen
            replay, recent errors and app details. Replays mask all text, media and links.
          </T>
        </DialogDescription>
      </DialogHeader>
      <DialogPanel>
        <div className="space-y-4">
          <div>
            <label
              htmlFor="bug-report-message"
              className="mb-1.5 block text-xs font-medium text-foreground"
            >
              <T>What happened?</T>
            </label>
            <Textarea
              id="bug-report-message"
              autoFocus
              value={message}
              onChange={(event) => setMessage(event.target.value)}
              placeholder={gt("What you did, what you expected, and what happened instead")}
              size="sm"
              onKeyDown={(event) => {
                if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                  event.preventDefault();
                  void submit();
                }
              }}
            />
          </div>
          <div>
            <label
              htmlFor="bug-report-email"
              className="mb-1.5 block text-xs font-medium text-foreground"
            >
              <T>Email (optional)</T>
            </label>
            <Input
              id="bug-report-email"
              type="email"
              size="sm"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              placeholder={gt("Only to reply about this report")}
            />
          </div>
          {screenshotUrl ? (
            <label className="flex items-start gap-3">
              <Checkbox
                checked={attachScreenshot}
                onCheckedChange={(checked) => setAttachScreenshot(checked === true)}
              />
              <span className="min-w-0 flex-1 space-y-2">
                <span className="block text-sm text-foreground">
                  <T>Attach a screenshot of this window</T>
                </span>
                <span className="block text-xs text-muted-foreground">
                  <T>Screenshots are not masked. Check that it shows nothing private.</T>
                </span>
                <img
                  src={screenshotUrl}
                  alt={gt("Screenshot of this window")}
                  className="max-h-40 rounded-md border border-border object-contain"
                />
              </span>
            </label>
          ) : null}
          {status === "failed" ? (
            <p className="text-xs text-destructive" role="alert">
              <T>The report could not be sent. Check your connection and try again.</T>
            </p>
          ) : null}
        </div>
      </DialogPanel>
      <DialogFooter variant="bare">
        <Button size="sm" variant="outline" type="button" onClick={() => closeBugReport(false)}>
          <T>Cancel</T>
        </Button>
        <Button size="sm" type="submit" disabled={trimmed.length === 0 || status === "sending"}>
          {status === "sending" ? <Spinner size="sm" /> : null}
          <T>Send report</T>
        </Button>
      </DialogFooter>
    </form>
  );
}
