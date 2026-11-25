import { thrownErrorMessage } from "@onicode/client-runtime/errors";
import { contextMenuSourceOf } from "../../contextMenuSource";
import {
  formatFilePathForClipboard,
  type MediaActionId,
} from "@onicode/client-runtime/media-actions";
import {
  mediaReferenceFileName,
  type MediaReference,
} from "@onicode/client-runtime/media-reference";
import { resolveAssetUrl } from "@onicode/client-runtime/state/assets";
import { settlePromise, squashAtomCommandFailure } from "@onicode/client-runtime/state/runtime";
import {
  AuthFilesystemReadScope,
  type AssetResource,
  type ContextMenuItem,
  type EnvironmentId,
  sessionGrantsScope,
  type SessionGrantInput,
  type ContextMenuSource,
} from "@onicode/contracts";
import { AsyncResult } from "effect/reactivity";
import { msg } from "gt-react";
import { useRef, useState, type ReactElement } from "react";

import { writeTextToClipboard } from "../../hooks/useCopyToClipboard";
import { readLocalApi } from "../../localApi";
import { appAtomRegistry } from "../../rpc/atomRegistry";
import { assetEnvironment } from "../../state/assets";
import { useEnvironmentQuery } from "../../state/query";
import { environmentSession, readPreparedConnection } from "../../state/session";
import { m } from "../../i18n/messages";
import { useAtomQueryRunner } from "../../state/use-atom-query-runner";
import { stackedThreadToast, toastManager } from "../ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { downloadMedia, readMediaPng } from "./mediaContent";

export interface MediaActionSource {
  readonly kind: "image" | "video";
  readonly name: string;
  readonly src: string | null;
  readonly reference?: MediaReference;
  readonly asset?: { readonly environmentId: EnvironmentId; readonly resource: AssetResource };
  readonly onOpenFile?: () => void;
}

function mediaFileName(source: MediaActionSource): string {
  return (
    (source.reference && mediaReferenceFileName(source.reference)) || source.name || source.kind
  );
}

function allowsHostMedia(session: SessionGrantInput | null) {
  return session === null || sessionGrantsScope(session, AuthFilesystemReadScope);
}

function canReadHostMedia(environmentId: EnvironmentId | null): boolean {
  if (environmentId === null) return true;
  const result = appAtomRegistry.get(environmentSession.sessionStateAtom(environmentId));
  return result._tag !== "Failure" && allowsHostMedia(AsyncResult.getOrElse(result, () => null));
}

async function runWithProgressToast(
  title: string,
  failureTitle: string,
  action: () => Promise<void>,
): Promise<void> {
  const progressToast = toastManager.add({ type: "loading", title });
  const result = await settlePromise(action);
  if (!AsyncResult.isFailure(result)) {
    toastManager.close(progressToast);
    return;
  }
  toastManager.update(
    progressToast,
    stackedThreadToast({
      type: "error",
      title: failureTitle,
      description:
        thrownErrorMessage(squashAtomCommandFailure(result), m) ??
        m(msg("The media action failed.")),
    }),
  );
}

export function useMediaActions(source: MediaActionSource) {
  const savesInFlight = useRef(new Set<string>());

  const hostEnvironmentId =
    source.asset &&
    (source.asset.resource._tag === "workspace-file" || source.asset.resource._tag === "media-file")
      ? source.asset.environmentId
      : null;
  const fileSession = useEnvironmentQuery(
    hostEnvironmentId === null ? null : environmentSession.sessionStateAtom(hostEnvironmentId),
  );
  const canReadMedia =
    hostEnvironmentId === null ||
    (fileSession.error === null && fileSession.data !== null && allowsHostMedia(fileSession.data));
  const assertCanReadMedia = () => {
    if (!canReadHostMedia(hostEnvironmentId)) {
      throw new Error(m(msg("This connection cannot read host files.")));
    }
  };
  const createAssetUrl = useAtomQueryRunner(assetEnvironment.createUrl, {
    reportFailure: false,
    refresh: true,
  });
  const actionUrl = async () => {
    assertCanReadMedia();
    if (!source.asset) {
      if (!source.src) {
        throw new Error(m(msg("This media is unavailable. Try reopening the preview.")));
      }
      return source.src;
    }
    const { environmentId, resource } = source.asset;
    const connection = readPreparedConnection(environmentId);
    if (!connection) throw new Error(m(msg("Reconnect to this environment and try again.")));
    const result = await createAssetUrl({ environmentId, input: { resource } });
    if (result._tag === "Failure") throw squashAtomCommandFailure(result);
    assertCanReadMedia();
    const url = resolveAssetUrl(connection.httpBaseUrl, result.value.relativeUrl);
    if (!url) throw new Error(m(msg("The environment returned an invalid media URL.")));
    return url;
  };
  const save = async () => {
    await downloadMedia(await actionUrl(), mediaFileName(source));
  };
  const canSave = canReadMedia && (source.src !== null || source.asset !== undefined);
  const saveWithToasts = async () => {
    const key = source.asset ? JSON.stringify(source.asset) : (source.src ?? "");
    const inFlight = savesInFlight.current;
    if (inFlight.has(key)) return;
    inFlight.add(key);
    const isImage = source.kind === "image";
    await runWithProgressToast(
      isImage ? m(msg("Preparing image download…")) : m(msg("Preparing video download…")),
      isImage ? m(msg("Could not save image")) : m(msg("Could not save video")),
      save,
    );
    inFlight.delete(key);
  };
  const copyImage = async () => {
    assertCanReadMedia();
    if (!navigator.clipboard?.write || typeof ClipboardItem === "undefined") {
      throw new Error(
        m(msg("Image copying is unavailable. Use a secure browser connection or save the image.")),
      );
    }
    // Start the clipboard write in the user gesture; fetching/decoding may finish later.
    await navigator.clipboard.write([
      new ClipboardItem({ "image/png": actionUrl().then(readMediaPng) }),
    ]);
  };
  return { save, saveWithToasts, canSave, copyImage, canReadMedia, assertCanReadMedia };
}

interface MediaActionsProps {
  source: MediaActionSource;
  children: ReactElement;
  showTooltip?: boolean;
}

export function MediaActions(props: MediaActionsProps) {
  return <MediaActionsMenu {...props} actions={useMediaActions(props.source)} />;
}

/** The media context menu for a caller that also offers the actions itself. */
export function MediaActionsMenu({
  source,
  actions,
  children,
  showTooltip = true,
}: MediaActionsProps & { actions: ReturnType<typeof useMediaActions> }) {
  const { saveWithToasts, canSave, copyImage, canReadMedia, assertCanReadMedia } = actions;
  const [tooltipOpen, setTooltipOpen] = useState(false);
  const menuOpen = useRef(false);
  const reference = source.reference;
  const tooltip = reference?.kind === "file" ? reference.path : (reference?.url ?? source.name);

  const showMenu = async (position: { x: number; y: number }, menuSource: ContextMenuSource) => {
    const api = readLocalApi();
    if (!api || menuOpen.current) return;
    menuOpen.current = true;
    setTooltipOpen(false);
    let failureTitle = m(msg("Could not open media menu"));
    const result = await settlePromise(async () => {
      const isImage = source.kind === "image";
      const canCopyImage =
        typeof navigator !== "undefined" &&
        Boolean(navigator.clipboard?.write) &&
        typeof ClipboardItem !== "undefined";
      const items: ContextMenuItem<MediaActionId>[] = [];
      if (reference?.kind === "file") {
        items.push({ id: "copy-full-path", label: m(msg("Copy full path")) });
        if (reference.relativePath)
          items.push({ id: "copy-relative-path", label: m(msg("Copy relative path")) });
      } else if (reference?.kind === "url") {
        items.push({ id: "copy-url", label: m(msg("Copy URL")) });
      }
      if (source.onOpenFile)
        items.push({
          id: "open-file",
          label: m(msg("Open in file viewer")),
          disabled: !canReadMedia,
        });
      items.push({
        id: "save",
        label: isImage ? m(msg("Save image")) : m(msg("Save video")),
        disabled: !canSave,
      });
      if (source.kind === "image") {
        items.push({
          id: "copy-image",
          label: m(msg("Copy image")),
          disabled: !canSave || !canCopyImage,
        });
      }

      const action = await api.contextMenu.show(items, position, menuSource);
      if (!action) return;
      failureTitle =
        action === "copy-full-path"
          ? m(msg("Could not copy full path"))
          : action === "copy-relative-path"
            ? m(msg("Could not copy relative path"))
            : action === "copy-url"
              ? m(msg("Could not copy url"))
              : action === "open-file"
                ? m(msg("Could not open in file viewer"))
                : m(msg("Could not complete media action"));
      const text =
        action === "copy-full-path" && reference?.kind === "file"
          ? reference.path
          : action === "copy-relative-path" && reference?.kind === "file"
            ? reference.relativePath
            : action === "copy-url" && reference?.kind === "url"
              ? reference.url
              : undefined;
      if (text !== undefined) {
        const clipboardText =
          reference?.kind === "file" ? formatFilePathForClipboard(text, reference.path) : text;
        await writeTextToClipboard(
          clipboardText,
          reference?.kind === "file"
            ? {
                target: "file path",
                unavailableMessage: msg("Clipboard API is unavailable while copying file path."),
                failureMessage: msg("Failed to copy file path to the clipboard."),
              }
            : {
                target: "URL",
                unavailableMessage: msg("Clipboard API is unavailable while copying URL."),
                failureMessage: msg("Failed to copy URL to the clipboard."),
              },
        );
      } else if (action === "open-file") {
        assertCanReadMedia();
        source.onOpenFile?.();
      } else if (action === "save") {
        await saveWithToasts();
      } else if (action === "copy-image") {
        await runWithProgressToast(
          m(msg("Copying image…")),
          m(msg("Could not copy image")),
          copyImage,
        );
      }
    });
    if (AsyncResult.isFailure(result)) {
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: failureTitle,
          description:
            thrownErrorMessage(squashAtomCommandFailure(result), m) ??
            m(msg("The media action failed.")),
        }),
      );
    }
    menuOpen.current = false;
  };

  return (
    <Tooltip disabled={!showTooltip} open={tooltipOpen} onOpenChange={setTooltipOpen}>
      <TooltipTrigger
        render={children}
        tabIndex={0}
        onContextMenu={(event) => {
          if (event.defaultPrevented) return;
          event.preventDefault();
          event.stopPropagation();
          const bounds = event.currentTarget.getBoundingClientRect();
          void showMenu(
            event.clientX === 0 && event.clientY === 0
              ? { x: bounds.left, y: bounds.bottom }
              : { x: event.clientX, y: event.clientY },
            contextMenuSourceOf(event),
          );
        }}
        onKeyDown={(event) => {
          if (
            event.defaultPrevented ||
            !(event.key === "ContextMenu" || (event.shiftKey && event.key === "F10"))
          )
            return;
          event.preventDefault();
          event.stopPropagation();
          const bounds = event.currentTarget.getBoundingClientRect();
          void showMenu({ x: bounds.left, y: bounds.bottom }, "keyboard");
        }}
      />
      <TooltipPopup variant="code">{tooltip}</TooltipPopup>
    </Tooltip>
  );
}
