import type { EnvironmentId } from "@onicode/contracts";
import { T, useGT, useMessages, Var } from "gt-react";
import { useRef, useState } from "react";

import { useProjectScans } from "../../onboarding/useProjectScans";
import type { WelcomeImportSelection } from "../../onboarding/welcomeState";
import {
  ImportSelectionList,
  reportImportResult,
  SCAN_LIMIT_MESSAGE,
  useImportSourcesLabel,
  useProjectImport,
} from "../onboarding/WelcomePage";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogAction,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "../ui/dialog";
import { toastManager } from "../ui/toast";
import { SettingsRow } from "./settingsLayout";
import { useSearchableSetting } from "./settingsSearch";

// The Welcome page checks recent projects for a new install. Here every project starts unchecked.
const NOTHING_SELECTED: WelcomeImportSelection = { knownKeys: [], keys: [], optedOut: true };

/** Imports agent history into projects after setup, from the same scan as the Welcome page. */
export function ImportThreadsRow({ environmentId }: { readonly environmentId: EnvironmentId }) {
  const setting = useSearchableSetting();
  const { label: importSourcesLabel } = useImportSourcesLabel(environmentId);
  const [open, setOpen] = useState(false);
  const [isImporting, setIsImporting] = useState(false);
  return (
    <SettingsRow
      {...setting("import-threads")}
      description={
        <T>
          Bring in projects and threads from your <Var>{importSourcesLabel}</Var> history, and
          continue them here.
        </T>
      }
      control={
        <>
          <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
            <T>Import…</T>
          </Button>
          <Dialog
            open={open}
            onOpenChange={(next) => {
              // Closing would drop the results of an import the server still finishes.
              if (next || !isImporting) setOpen(next);
            }}
          >
            <DialogPopup className="max-w-2xl" showCloseButton={!isImporting}>
              {open ? (
                <ImportThreadsDialogBody
                  environmentId={environmentId}
                  isImporting={isImporting}
                  setIsImporting={setIsImporting}
                  onClose={() => {
                    setIsImporting(false);
                    setOpen(false);
                  }}
                />
              ) : null}
            </DialogPopup>
          </Dialog>
        </>
      }
    />
  );
}

function ImportThreadsDialogBody({
  environmentId,
  isImporting,
  setIsImporting,
  onClose,
}: {
  readonly environmentId: EnvironmentId;
  readonly isImporting: boolean;
  readonly setIsImporting: (value: boolean) => void;
  readonly onClose: () => void;
}) {
  const gt = useGT();
  const m = useMessages();
  const { label: importSourcesLabel } = useImportSourcesLabel(environmentId);
  const scans = useProjectScans([environmentId]);
  const scan = scans[0];
  const [importSelection, setImportSelection] = useState(NOTHING_SELECTED);
  // The dialog blocks its own close while importing, so nothing reads this hold.
  const holdPageRef = useRef(false);
  const projectImport = useProjectImport({
    scans,
    importSelection,
    onImportSelectionChange: setImportSelection,
    isImporting,
    setIsImporting,
    holdPageRef,
    onDone: async (_projectRef, importWarning, importedThreadCount = 0) => {
      if (!reportImportResult(gt, importWarning, importedThreadCount)) {
        toastManager.add({ type: "info", title: gt("No new threads to import") });
      }
      onClose();
      return true;
    },
  });
  const { candidates, canWrite, selected, visibleImportError } = projectImport;

  return (
    <>
      <DialogHeader>
        <DialogTitle>
          <T>Import threads</T>
        </DialogTitle>
        <DialogDescription>
          <T>
            Pick projects from your <Var>{importSourcesLabel}</Var> history. Threads already in
            Onicode are not imported again.
          </T>
        </DialogDescription>
      </DialogHeader>
      <div className="min-w-0 px-4 pb-2">
        {scan === undefined || (scan.isPending && candidates.length === 0) ? (
          <p className="px-2 text-sm text-muted-foreground" role="status">
            <T>
              Looking for projects in your <Var>{importSourcesLabel}</Var> history…
            </T>
          </p>
        ) : candidates.length === 0 && scan.error === null ? (
          <p className="px-2 text-sm text-muted-foreground" role="status">
            {scan.data?.truncated ? m(SCAN_LIMIT_MESSAGE) : gt("No agent history found.")}
          </p>
        ) : (
          <>
            {candidates.length > 0 ? (
              <fieldset className="min-w-0" disabled={isImporting || !canWrite}>
                <ImportSelectionList projectImport={projectImport} />
              </fieldset>
            ) : null}
            {scan.data?.truncated ? (
              <p className="px-2 pt-2 text-xs text-muted-foreground" role="status">
                {m(SCAN_LIMIT_MESSAGE)}
              </p>
            ) : null}
            {scan.error !== null ? (
              <div
                role="alert"
                className="flex items-center justify-between gap-3 px-2 pt-2 text-sm text-muted-foreground"
              >
                <span>{gt("Could not check projects. {error}", { error: scan.error })}</span>
                <Button variant="ghost" size="sm" onClick={scan.refresh}>
                  <T>Retry</T>
                </Button>
              </div>
            ) : null}
            {visibleImportError ? (
              <p className="px-2 pt-2 text-sm text-destructive">{m(visibleImportError)}</p>
            ) : null}
          </>
        )}
      </div>
      <DialogFooter variant="bare">
        <Button variant="outline" disabled={isImporting} onClick={onClose}>
          <T>Cancel</T>
        </Button>
        <DialogAction
          disabled={isImporting || !canWrite || selected.length === 0}
          onClick={() => void projectImport.runImport(selected)}
        >
          {isImporting
            ? gt("Importing…")
            : selected.length === 0
              ? gt("Import")
              : gt(
                  "{count, plural, one {Import {count} project} other {Import {count} projects}}",
                  { count: selected.length },
                )}
        </DialogAction>
      </DialogFooter>
    </>
  );
}
