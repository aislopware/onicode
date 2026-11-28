import {
  memo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
  type KeyboardEvent,
  type MouseEvent,
} from "react";
import { flushSync } from "react-dom";
import { useGT } from "gt-react";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  DownloadIcon,
  ImageIcon,
  PencilIcon,
  TextIcon,
  XIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react";
import { Button } from "../ui/button";
import { Dialog, DialogPopup, DialogTitle } from "../ui/dialog";
import type { ExpandedImageItem, ExpandedImagePreview } from "./ExpandedImagePreview";
import { resolveExternalWebLinkHost } from "./externalLinkContextMenu";
import { useAssetUrlRefresh, useAssetUrlState } from "../../assets/assetUrls";
import { OpenMediaLink } from "../media/OpenMediaLink";
import { MediaActionsMenu, useMediaActions, type MediaActionSource } from "../media/MediaActions";
import { MediaVideoPlayer } from "../media/MediaVideoPlayer";
import { isContextMenuOpen } from "../../contextMenuFallback";
import {
  SnapShotAccessibilityData,
  SnapShotCopyButton,
  SnapShotContentsButton,
  snapShotAccessibilityDetails,
} from "./SnapShotAttachmentDetails";
import { MiddleTruncate } from "../ui/middle-truncate";
import { SharedTooltipTrigger } from "../ui/shared-tooltip";
import {
  ZoomableImage,
  zoomableImageFit,
  zoomableImageMaxZoom,
  zoomableImageScale,
  type NaturalSize,
  type ZoomableImageBounds,
  type ZoomableImageHandle,
} from "./ZoomableImage";
import {
  type EditedImage,
  ImageAnnotationOverlay,
  type ImageAnnotationStatus,
} from "./ImageAnnotationOverlay";
import { composerFloatingLayerProps } from "./composerEventScope";
import { useAtomValue } from "@effect/atom-react";
import { Atom } from "effect/reactivity";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import { expandedImageKey } from "./ExpandedImagePreview";
import { registerThreadOverlay } from "./threadOverlayRetention";
import { observeResize } from "~/lib/observeResize";
import { mediaReferenceFileName } from "@onicode/client-runtime/media-reference";
import { mediaMimeType, mediaMimeTypeFromExtension } from "@onicode/shared/mediaPreview";

interface ExpandedImageDialogProps {
  preview: ExpandedImagePreview;
  onClose: () => void;
}

const EXPANDED_MEDIA_STATE_CLASS_NAME =
  "flex aspect-auto h-48 min-h-0 w-[min(var(--media-width),32rem)] flex-col items-center justify-center gap-3 rounded-lg border border-border bg-black p-6 text-center text-sm text-white shadow-2xl";

function ExpandedMediaFailure({ children }: { children: ReactNode }) {
  return (
    <div role="alert" className={EXPANDED_MEDIA_STATE_CLASS_NAME}>
      {children}
    </div>
  );
}

function itemMimeType(item: ExpandedImageItem): string | undefined {
  if (item.mimeType) return item.mimeType;
  const reference = item.actionsSource?.reference;
  const fileName = reference && mediaReferenceFileName(reference);
  if (fileName) {
    return mediaMimeTypeFromExtension(fileName.slice(fileName.lastIndexOf("."))) ?? undefined;
  }
  return mediaMimeType(item.src ?? "") ?? undefined;
}

function ExpandedVideo({ item }: { readonly item: ExpandedImageItem }) {
  const asset = item.actionsSource?.asset;
  const assetUrl = useAssetUrlState(asset?.environmentId ?? null, asset?.resource ?? null);
  const refreshAssetUrl = useAssetUrlRefresh(asset?.environmentId ?? null, asset?.resource ?? null);
  const src = asset
    ? assetUrl._tag === "Success"
      ? assetUrl.url + (item.srcFragment ?? "")
      : null
    : item.src;
  return (
    <MediaVideoPlayer
      src={src}
      label={item.name}
      sourceFailed={assetUrl._tag === "Failure"}
      originalUrl={item.originalUrl}
      preload="metadata"
      autoPlay={item.autoPlay ?? true}
      className="block max-h-[var(--media-height)] max-w-[var(--media-width)] text-center"
      videoClassName="aspect-auto max-h-[var(--media-height)] w-auto max-w-[var(--media-width)] rounded-lg border border-border shadow-2xl"
      stateClassName={EXPANDED_MEDIA_STATE_CLASS_NAME}
      onRetry={asset ? refreshAssetUrl : undefined}
    />
  );
}

export const ExpandedImageDialog = memo(function ExpandedImageDialog({
  preview,
  onClose,
}: ExpandedImageDialogProps) {
  const gt = useGT();
  const [imageOffset, setImageOffset] = useState(0);
  const [failedImageSrc, setFailedImageSrc] = useState<string | null>(null);
  const [accessibilityDetailsSrc, setAccessibilityDetailsSrc] = useState<string | null>(null);
  const zoomableImageRef = useRef<ZoomableImageHandle>(null);
  const [zoom, setZoom] = useState(1);
  // Drawing happens in place over the current image, so the preview around it never moves.
  const [editing, setEditing] = useState(false);
  const [editStatus, setEditStatus] = useState<ImageAnnotationStatus>({
    dirty: false,
    saving: false,
  });
  // A saved edit replaces the draft's bytes; show them here without reopening the preview.
  const [editedImages, setEditedImages] = useState<Readonly<Record<number, EditedImage>>>({});
  const editButtonRef = useRef<HTMLButtonElement>(null);
  const [natural, setNatural] = useState<{ src: string; size: NaturalSize } | null>(null);
  const [stage, setStage] = useState<ZoomableImageBounds | null>(null);
  const [zoomed, setZoomed] = useState<{ stage: ZoomableImageBounds | null; view: string | null }>({
    stage: null,
    view: null,
  });
  const attachStage = (element: HTMLDivElement) => {
    const measure = () => {
      // Side gutters keep the edge navigation buttons off the media.
      const gutter = window.innerWidth >= 640 ? 96 : 0;
      const width = Math.max(1, element.clientWidth - gutter);
      const height = Math.max(1, element.clientHeight);
      setStage((current) =>
        current?.width === width && current.height === height ? current : { width, height },
      );
    };
    measure();
    return observeResize(element, measure);
  };
  const [returnFocus] = useState(() => {
    const target = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    return { target, focusVisible: target?.matches(":focus-visible") ?? false };
  });
  // The host unmounts the dialog outright, so Base UI never runs finalFocus. Without
  // this, focus stays on the removed dialog, and the composer that owned the opener
  // reads that as a blur and rests. Preserve mouse focus when Escape closes it.
  const close = () => {
    flushSync(onClose);
    const { target, focusVisible } = returnFocus;
    if (target?.isConnected) target.focus({ preventScroll: true, focusVisible });
  };
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const imageCount = preview.images.length;
  const index =
    imageCount > 0 ? (((preview.index + imageOffset) % imageCount) + imageCount) % imageCount : 0;
  const previewItem = preview.images[index];
  const editedImage = editedImages[index];
  const item =
    previewItem && editedImage
      ? {
          ...previewItem,
          src: editedImage.src,
          ...(previewItem.edit ? { edit: { ...previewItem.edit, file: editedImage.file } } : {}),
        }
      : previewItem;
  const source: MediaActionSource = item?.actionsSource ?? {
    kind: item?.type === "video" ? "video" : "image",
    name: item?.name ?? gt("Media"),
    src: item?.src ?? null,
  };
  const openFile = source.onOpenFile;
  const actionsSource: MediaActionSource = openFile
    ? {
        ...source,
        onOpenFile: () => {
          openFile();
          close();
        },
      }
    : source;

  const mediaActions = useMediaActions(actionsSource);
  const { canSave } = mediaActions;

  const exitEdit = () => {
    setEditing(false);
    setEditStatus({ dirty: false, saving: false });
    editButtonRef.current?.focus({ preventScroll: true });
  };
  const cancelEdit = () => {
    if (!editStatus.saving) exitEdit();
  };

  const navigateImage = (direction: -1 | 1) => {
    if (editing) {
      if (editStatus.dirty) return;
      exitEdit();
    }
    setImageOffset((current) => current + direction);
  };

  const closeFromBackdrop = (event: MouseEvent) => {
    if (event.target === event.currentTarget && !editing) close();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.defaultPrevented || isContextMenuOpen() || event.target instanceof HTMLVideoElement)
      return;
    if (!editing && zoomableImageRef.current?.pan(event.key)) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (preview.images.length <= 1) return;
    if (event.key === "ArrowLeft") {
      event.preventDefault();
      event.stopPropagation();
      navigateImage(-1);
      return;
    }
    if (event.key !== "ArrowRight") return;
    event.preventDefault();
    event.stopPropagation();
    navigateImage(1);
  };

  if (!item) return null;
  const isVideo = item.type === "video";
  const editTarget = editing && !isVideo ? item.edit : undefined;
  const openOriginalLink =
    item.originalUrl && resolveExternalWebLinkHost(item.originalUrl) !== null ? (
      <OpenMediaLink originalUrl={item.originalUrl} />
    ) : null;
  const accessibilityDetails = item.source ? snapShotAccessibilityDetails(item.source) : undefined;
  const showingAccessibilityDetails =
    Boolean(accessibilityDetails) && accessibilityDetailsSrc === item.src;
  const contentsLabel = showingAccessibilityDetails
    ? gt("Show screenshot")
    : accessibilityDetails?.format === "json"
      ? gt("Show accessibility JSON")
      : gt("Show extracted text");

  const naturalSize =
    natural !== null && natural.src === item.src ? natural.size : { width: 0, height: 0 };
  const showsZoomableImage =
    !isVideo && !showingAccessibilityDetails && item.src !== null && failedImageSrc !== item.src;
  // A resized stage refits the image, and a remounted one starts fitted.
  const imageKey = `${index}:${item.src}`;
  const view = showsZoomableImage ? imageKey : null;
  if (zoomed.stage !== stage || zoomed.view !== view) {
    setZoomed({ stage, view });
    setZoom(1);
  }
  const fit = stage ? zoomableImageFit(stage, naturalSize, itemMimeType(item)) : 1;
  const scalePercent = Math.round(zoomableImageScale(fit, zoom) * 100);
  const canShowActualSize = zoom === 1 && fit < 1 && naturalSize.width > 0;
  const zoomTo = (next: number) => zoomableImageRef.current?.zoomTo(next);
  const multiple = preview.images.length > 1;

  return (
    <Dialog
      open
      onOpenChange={(open, eventDetails) => {
        if (open) return;
        // Escape belongs to an open context menu first.
        if (eventDetails.reason === "escape-key" && isContextMenuOpen()) {
          eventDetails.cancel();
          return;
        }
        // While drawing, Escape cancels the drawing and nothing else closes the preview.
        if (editing) {
          eventDetails.cancel();
          if (eventDetails.reason === "escape-key") cancelEdit();
          return;
        }
        close();
      }}
    >
      <DialogPopup
        {...composerFloatingLayerProps}
        variant="media"
        mediaControlsPlacement="inside"
        showCloseButton={false}
        bottomStickOnMobile={false}
        className="row-start-1 h-full max-h-[92vh] w-[92vw] max-w-[92vw] overflow-visible"
        onKeyDown={onKeyDown}
        // Focus lands on Close either way, but only a keyboard open shows its ring.
        initialFocus={() => {
          closeButtonRef.current?.focus({
            preventScroll: true,
            focusVisible: returnFocus.focusVisible,
          });
          return false;
        }}
        finalFocus={() => returnFocus.target}
        onClick={closeFromBackdrop}
      >
        <DialogTitle className="sr-only">
          {isVideo ? gt("Expanded video preview") : gt("Expanded image preview")}
        </DialogTitle>
        <div className="mb-2 flex h-9 shrink-0 items-center gap-3" onClick={closeFromBackdrop}>
          {/* First in Tab order, and outside the media menu so its context menu is not the media's. */}
          <Button
            type="button"
            ref={closeButtonRef}
            size="icon-lg"
            variant="media-control"
            className="order-last"
            onClick={editing ? cancelEdit : close}
            aria-label={
              editing
                ? gt("Cancel drawing")
                : isVideo
                  ? gt("Close video preview")
                  : gt("Close image preview")
            }
          >
            <XIcon aria-hidden="true" />
          </Button>
          <div
            className="flex min-w-0 flex-1 items-center gap-2 text-sm"
            onClick={closeFromBackdrop}
          >
            <span className="flex min-w-0 items-center gap-2" aria-live="polite" aria-atomic="true">
              {multiple && (
                <span className="shrink-0 font-medium text-white tabular-nums">
                  {index + 1} / {preview.images.length}
                </span>
              )}
              <span className="flex max-w-xl min-w-0 text-white/60">
                <MiddleTruncate value={item.name} />
              </span>
            </span>
            {accessibilityDetails && item.source ? (
              <SharedTooltipTrigger
                render={
                  <Button
                    aria-label={contentsLabel}
                    aria-pressed={showingAccessibilityDetails}
                    disabled={editing}
                    onClick={() =>
                      setAccessibilityDetailsSrc(showingAccessibilityDetails ? null : item.src)
                    }
                    size="icon-sm"
                    variant="media-ghost"
                  />
                }
                label={contentsLabel}
                side="bottom"
              >
                {showingAccessibilityDetails ? (
                  <ImageIcon aria-hidden="true" />
                ) : (
                  <TextIcon aria-hidden="true" />
                )}
              </SharedTooltipTrigger>
            ) : item.source ? (
              <SnapShotContentsButton source={item.source} side="bottom" variant="media" />
            ) : null}
            {item.onCopySnapshot && (
              <SnapShotCopyButton onCopy={item.onCopySnapshot} side="bottom" variant="media" />
            )}
          </div>
        </div>
        <div
          ref={attachStage}
          className="flex min-h-0 flex-1 items-center justify-center"
          style={
            stage
              ? ({
                  "--media-width": `${stage.width}px`,
                  "--media-height": `${stage.height}px`,
                } as CSSProperties)
              : undefined
          }
          onClick={closeFromBackdrop}
        >
          {stage && (
            <MediaActionsMenu source={actionsSource} actions={mediaActions} showTooltip={false}>
              <div className="flex max-h-full max-w-[var(--media-width)] items-center justify-center">
                {item.type === "video" ? (
                  <ExpandedVideo key={index} item={item} />
                ) : showingAccessibilityDetails ? (
                  accessibilityDetails ? (
                    <SnapShotAccessibilityData
                      details={accessibilityDetails}
                      className="h-[min(var(--media-height),40rem)] w-[min(var(--media-width),42rem)] transition-opacity duration-150 ease-out-quint starting:opacity-0 rounded-lg border border-border bg-background p-4 text-xs leading-5 shadow-2xl motion-reduce:transition-none"
                    />
                  ) : null
                ) : item.src === null || failedImageSrc === item.src ? (
                  <ExpandedMediaFailure>
                    <p>
                      {openOriginalLink
                        ? gt("This image could not be loaded.")
                        : gt("Image unavailable. The file may have been moved or deleted.")}
                    </p>
                    {openOriginalLink}
                  </ExpandedMediaFailure>
                ) : (
                  <ZoomableImage
                    ref={zoomableImageRef}
                    key={imageKey}
                    src={item.src}
                    name={item.name}
                    mimeType={itemMimeType(item)}
                    bounds={stage}
                    naturalSize={naturalSize}
                    onNaturalSize={(size) => {
                      if (item.src !== null) setNatural({ src: item.src, size });
                    }}
                    zoom={zoom}
                    onZoomChange={setZoom}
                    onError={() => setFailedImageSrc(item.src)}
                    overlay={
                      editTarget ? (
                        <ImageAnnotationOverlay
                          target={editTarget}
                          onStatusChange={setEditStatus}
                          onSaved={(edited) => {
                            setEditedImages((current) => ({ ...current, [index]: edited }));
                            exitEdit();
                          }}
                          onExit={exitEdit}
                        />
                      ) : undefined
                    }
                  />
                )}
              </div>
            </MediaActionsMenu>
          )}
        </div>
        <div
          className="mt-2 flex h-10 shrink-0 items-center justify-center max-sm:px-12"
          onClick={closeFromBackdrop}
        >
          {(showsZoomableImage || canSave) && (
            <div
              role="toolbar"
              aria-label={gt("Media controls")}
              className="flex items-center gap-0.5 rounded-full bg-white/8 p-0.5 ring-1 ring-white/10"
            >
              {showsZoomableImage && item.edit ? (
                <>
                  <SharedTooltipTrigger
                    render={
                      <Button
                        ref={editButtonRef}
                        size="icon-sm"
                        variant="media-ghost"
                        aria-label={gt("Draw on image")}
                        aria-pressed={editing}
                        disabled={editStatus.saving}
                        onClick={() => (editing ? cancelEdit() : setEditing(true))}
                      />
                    }
                    label={gt("Draw on image")}
                  >
                    <PencilIcon aria-hidden="true" />
                  </SharedTooltipTrigger>
                  <span aria-hidden="true" className="mx-1 h-4 w-px bg-white/15" />
                </>
              ) : null}
              {showsZoomableImage && (
                <>
                  <Button
                    size="icon-sm"
                    variant="media-ghost"
                    aria-label={gt("Zoom out")}
                    disabled={editing || zoom <= 1}
                    onClick={() => zoomTo(zoom / 1.5)}
                  >
                    <ZoomOutIcon aria-hidden="true" />
                  </Button>
                  <Button
                    size="sm-pill"
                    variant="media-ghost"
                    className="min-w-14"
                    aria-label={zoom > 1 ? gt("Fit to screen") : gt("Actual size")}
                    disabled={editing || (zoom === 1 && !canShowActualSize)}
                    onClick={() => zoomTo(zoom > 1 ? 1 : 1 / fit)}
                  >
                    <span className="tabular-nums">
                      {naturalSize.width > 0 ? `${scalePercent}%` : "–"}
                    </span>
                  </Button>
                  <Button
                    size="icon-sm"
                    variant="media-ghost"
                    aria-label={gt("Zoom in")}
                    disabled={
                      editing || naturalSize.width === 0 || zoom >= zoomableImageMaxZoom(fit)
                    }
                    onClick={() => zoomTo(zoom * 1.5)}
                  >
                    <ZoomInIcon aria-hidden="true" />
                  </Button>
                </>
              )}
              {showsZoomableImage && canSave && (
                <span aria-hidden="true" className="mx-1 h-4 w-px bg-white/15" />
              )}
              {canSave && (
                <Button
                  size="icon-sm"
                  variant="media-ghost"
                  aria-label={isVideo ? gt("Save video") : gt("Save image")}
                  onClick={() => void mediaActions.saveWithToasts()}
                >
                  <DownloadIcon aria-hidden="true" />
                </Button>
              )}
            </div>
          )}
        </div>
        {multiple && (
          <>
            <Button
              type="button"
              size="icon-lg"
              variant="media-control"
              className="absolute bottom-0 left-0 z-20 sm:top-1/2 sm:bottom-auto sm:-translate-y-1/2"
              aria-label={gt("Previous media")}
              disabled={editing && editStatus.dirty}
              onClick={() => navigateImage(-1)}
            >
              <ChevronLeftIcon className="size-5" />
            </Button>
            <Button
              type="button"
              size="icon-lg"
              variant="media-control"
              className="absolute right-0 bottom-0 z-20 sm:top-1/2 sm:bottom-auto sm:-translate-y-1/2"
              aria-label={gt("Next media")}
              disabled={editing && editStatus.dirty}
              onClick={() => navigateImage(1)}
            >
              <ChevronRightIcon className="size-5" />
            </Button>
          </>
        )}
      </DialogPopup>
    </Dialog>
  );
});

// Keep preview state outside ChatView to isolate dialog renders.
type ExpandedImagePreviewState = {
  readonly threadKey: string;
  readonly preview: ExpandedImagePreview;
} | null;

const expandedImagePreviewAtom = Atom.make<ExpandedImagePreviewState>(null).pipe(
  Atom.keepAlive,
  Atom.withLabel("chat:expanded-image-preview"),
);

function ownedSrc(state: ExpandedImagePreviewState): string | null {
  const item = state?.preview.images[state.preview.index];
  return item?.ownsSrc === true ? item.src : null;
}

// The preview owns a blob URL made for it, so whatever replaces it releases that URL.
function setExpandedImagePreview(next: ExpandedImagePreviewState): void {
  const previous = ownedSrc(appAtomRegistry.get(expandedImagePreviewAtom));
  appAtomRegistry.set(expandedImagePreviewAtom, next);
  if (previous !== null && previous !== ownedSrc(next)) URL.revokeObjectURL(previous);
}

export function openExpandedImagePreview(threadKey: string, preview: ExpandedImagePreview): void {
  setExpandedImagePreview({ threadKey, preview });
}

function closeExpandedImagePreview(): void {
  setExpandedImagePreview(null);
}

// Leaving the thread's route (or the chat view) drops its preview.
registerThreadOverlay((threadKey) => {
  const current = appAtomRegistry.get(expandedImagePreviewAtom);
  if (current !== null && current.threadKey !== threadKey) closeExpandedImagePreview();
});

export function ExpandedImageDialogHost({ threadKey }: { readonly threadKey: string }) {
  const state = useAtomValue(expandedImagePreviewAtom);
  const preview = state?.threadKey === threadKey ? state.preview : null;
  if (preview === null) return null;
  return (
    <ExpandedImageDialog
      key={expandedImageKey(preview)}
      preview={preview}
      onClose={closeExpandedImagePreview}
    />
  );
}
