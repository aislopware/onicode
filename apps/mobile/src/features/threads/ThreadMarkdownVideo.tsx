import { useGT } from "gt-react-native";
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
/** Asks the feed to keep this row out of list recycling while its media holds playback state. */
export const ThreadMediaRetainContext = createContext<(retained: boolean) => void>(() => {});

export function ThreadMarkdownVideo(props: {
  readonly source: MediaVideoPreviewSource;
  /** Keeps the player and its poster across source changes that show the same video. */
  readonly thumbnailKey?: string;
  readonly thumbnailVisible?: boolean;
  readonly onPlaybackStart?: () => void;
  readonly borderRadius?: number;
}) {
  const gt = useGT();
  const { source } = props;
  const feedVisible = useThreadMediaVisible();
  const visible = props.thumbnailVisible ?? feedVisible;
  const thumbnailKey = props.thumbnailKey ?? mediaVideoThumbnailKey(source);
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
          ? gt("This connection cannot read host files. Pair again with a new link to view it.")
          : undefined
      }
      actionsSource={source.actionsSource}
      borderRadius={props.borderRadius}
    />
  );
}
