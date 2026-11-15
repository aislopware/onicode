import { MISSING_FILE_ACCESS_MESSAGE } from "@onicode/client-runtime/state/assets";
import { createContext, use, useSyncExternalStore } from "react";
import { MediaVideoPlayer } from "../../components/MediaVideoPlayer";
import { useAssetUrlState, useRefreshAssetUrl } from "../../state/assets";
import {
  mediaVideoPreviewUri,
  mediaVideoThumbnailKey,
  type MediaVideoPreviewSource,
} from "../../lib/videoPreviewSource";

/**
 * Whether a feed row is on screen. A store instead of a context value, so a row
 * with no media does not render when it scrolls in or out.
 */
export function createThreadMediaVisibility() {
  let visible = false;
  const listeners = new Set<() => void>();
  return {
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => void listeners.delete(listener);
    },
    isVisible: () => visible,
    set(next: boolean) {
      if (next === visible) return;
      visible = next;
      for (const listener of listeners) listener();
    },
  };
}

type ThreadMediaVisibility = Pick<
  ReturnType<typeof createThreadMediaVisibility>,
  "subscribe" | "isVisible"
>;

export const ThreadMediaVisibleContext = createContext<ThreadMediaVisibility | null>(null);

const subscribeNever = () => () => {};

export function useThreadMediaVisible(): boolean {
  const visibility = use(ThreadMediaVisibleContext);
  const isVisible = () => visibility?.isVisible() ?? false;
  return useSyncExternalStore(visibility?.subscribe ?? subscribeNever, isVisible, isVisible);
}

export function ThreadMarkdownVideo(props: {
  readonly source: MediaVideoPreviewSource;
  readonly thumbnailVisible?: boolean;
  readonly onPlaybackStart?: () => void;
  readonly borderRadius?: number;
}) {
  const { source } = props;
  const feedVisible = useThreadMediaVisible();
  const visible = props.thumbnailVisible ?? feedVisible;
  const thumbnailKey = mediaVideoThumbnailKey(source);
  const asset = useAssetUrlState(
    "environmentId" in source ? source.environmentId : null,
    "resource" in source ? source.resource : null,
  );
  const refreshAssetUrl = useRefreshAssetUrl(
    "environmentId" in source ? source.environmentId : null,
    "resource" in source ? source.resource : null,
  );
  const uri = mediaVideoPreviewUri(source, asset._tag === "Success" ? asset.url : null);
  return (
    <MediaVideoPlayer
      key={thumbnailKey}
      uri={uri}
      resolvePlaybackUri={
        "resource" in source
          ? async () => mediaVideoPreviewUri(source, await refreshAssetUrl())
          : undefined
      }
      name={source.name}
      thumbnailKey={thumbnailKey}
      thumbnailVisible={visible}
      visible={visible}
      onPlaybackStart={props.onPlaybackStart}
      unavailable={"resource" in source && asset._tag === "Failure"}
      unavailableDetail={
        asset._tag === "Failure" && asset.reason === "missing-file-access"
          ? MISSING_FILE_ACCESS_MESSAGE
          : undefined
      }
      actionsSource={source.actionsSource}
      borderRadius={props.borderRadius}
    />
  );
}
