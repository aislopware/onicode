import {
  splitPullRequestBody,
  type PullRequestBodySegment,
} from "@onicode/client-runtime/pull-request-markdown";
import { markdownTaskMarkerOffset } from "@onicode/shared/markdownTaskList";
import type { ReactNode } from "react";
import { View } from "react-native";
import { StackActions, useNavigation } from "@react-navigation/native";
import { useProject } from "../../state/entities";
import { pullRequestTarget, type PullRequestParams } from "./usePullRequestDiff";
import {
  PullRequestMedia,
  PullRequestMediaContext,
  usePullRequestMediaUrl,
} from "./PullRequestMedia";
import { videoMimeType } from "@onicode/shared/video";
import { createContext, useContext, useMemo, useState } from "react";
import {
  changeRequestFileBase,
  changeRequestReferenceAutolinker,
  resolveChangeRequestFileHref,
  resolveChangeRequestMediaUrl,
} from "@onicode/client-runtime/pull-request-references";
import {
  sourceControlClients,
  type ReferenceAutolink,
} from "@onicode/client-runtime/source-control-clients";
import { nativePullRequestMarkdown } from "./pullRequestAutolinks";

import { useOpenPullRequest } from "../../native/useOpenPullRequest";
import { pullRequestFileLinkRoute } from "../../lib/pullRequestNavigation";
import { SelectableMarkdownText } from "../../native/SelectableMarkdownText";
import { useMarkdownPreviewStyles } from "../files/FileMarkdownPreview";
import { markdownImageSources } from "../../lib/markdownImageSources";
import { markdownReferenceDefinitions } from "../../lib/markdownDocument";
import { FilePreviewGallery, useFilePreviewGallery } from "../../components/ui/FilePreviewGallery";
import { pullRequestImageSource } from "./pullRequestMediaSource";
import {
  FilePreviewModal,
  type FilePreviewSource,
  type FilePreviewItem,
} from "../../components/ui/FilePreviewModal";
import { ThreadMarkdownImageView } from "../threads/ThreadMarkdownImage";
import type { MarkdownImageRenderer } from "../../native/SelectableMarkdownText";
import type { SelectableMarkdownTextProps } from "../../native/SelectableMarkdownText.types";

const AutolinksContext = createContext<((text: string) => ReadonlyArray<ReferenceAutolink>) | null>(
  null,
);
const LineBreaksContext = createContext(false);
const GithubEmojiContext = createContext(false);
const ChangeRequestUrlContext = createContext<string | null>(null);
const FileLinkCheckoutContext = createContext<Parameters<typeof pullRequestFileLinkRoute>[1]>(null);
const FileBaseContext = createContext<string | null>(null);

export function PullRequestMarkdownRepository(props: {
  /** The change request the markdown belongs to, whose host decides what its text links. */
  readonly changeRequest:
    | {
        readonly provider: string;
        readonly url: string;
        /** The checkout the server read it through, where its file links open. */
        readonly workspaceRoot?: string | undefined;
        readonly borrowedCheckout?: boolean | undefined;
        /** Path links the checkout cannot open show on the forge at this commit. */
        readonly headSha?: string | undefined;
      }
    | null
    | undefined;
  readonly params: PullRequestParams;
  readonly children: ReactNode;
}) {
  const provider = props.changeRequest?.provider;
  const url = props.changeRequest?.url;
  const autolinks =
    provider === undefined || url === undefined
      ? null
      : changeRequestReferenceAutolinker({ provider, url });
  const lineBreaks = sourceControlClients.find(provider ?? "")?.hardLineBreaks ?? false;
  const target = pullRequestTarget(props.params);
  const project = useProject({
    environmentId: target.environmentId,
    projectId: target.input.projectId,
  });
  const cwd = project?.workspaceRoot;
  const media = cwd
    ? { environmentId: target.environmentId, cwd, repository: target.input.repository }
    : null;
  const workspaceRoot = props.changeRequest?.workspaceRoot;
  const fileLinkCheckout =
    workspaceRoot === undefined
      ? null
      : {
          environmentId: props.params.environmentId,
          workspaceRoot,
          borrowedCheckout: props.changeRequest?.borrowedCheckout,
        };
  const fileBase =
    props.changeRequest && (fileLinkCheckout === null || fileLinkCheckout.borrowedCheckout)
      ? changeRequestFileBase(props.changeRequest)
      : null;
  return (
    <AutolinksContext value={autolinks}>
      <GithubEmojiContext value={provider === "github" || provider === "gitlab"}>
        <LineBreaksContext value={lineBreaks}>
          <ChangeRequestUrlContext value={url ?? null}>
            <FileLinkCheckoutContext value={fileLinkCheckout}>
              <FileBaseContext value={fileBase}>
                <PullRequestMediaContext value={media}>{props.children}</PullRequestMediaContext>
              </FileBaseContext>
            </FileLinkCheckoutContext>
          </ChangeRequestUrlContext>
        </LineBreaksContext>
      </GithubEmojiContext>
    </AutolinksContext>
  );
}

function MarkdownImage(props: {
  readonly href: string;
  readonly alt: string | null;
  readonly onPreview: (source: FilePreviewSource) => void;
}) {
  const media = usePullRequestMediaUrl(props.href);
  const context = useContext(PullRequestMediaContext);
  const withGallery = useFilePreviewGallery();
  return (
    <ThreadMarkdownImageView
      uri={media.uri}
      sourceKey={props.href}
      cacheKey={media.cacheKey}
      unavailable={media.unavailable || !/^https?:\/\//i.test(props.href)}
      alt={props.alt}
      onPressPreview={(source) =>
        props.onPreview(
          withGallery({
            ...pullRequestImageSource(props.href, context),
            name: source.name,
            sourceIdentifier: source.sourceIdentifier,
          }),
        )
      }
    />
  );
}

export function PullRequestMarkdown(props: {
  readonly markdown: string;
  readonly imageGroupMarkdown?: string;
  readonly lineBreaks?: boolean;
  /**
   * `markerOffset` is into `markdown`. Returning false, or a tap that can't be traced back to
   * the source, undoes the tick the renderer already drew.
   */
  readonly onTaskListChange?: (input: {
    readonly markerOffset: number;
    readonly checked: boolean;
  }) => boolean;
}) {
  const mediaContext = useContext(PullRequestMediaContext);
  const changeRequestUrl = useContext(ChangeRequestUrlContext);
  const segments = useMemo(
    () => splitPullRequestBody(props.markdown, markdownReferenceDefinitions),
    [props.markdown],
  );
  const [undoneTicks, setUndoneTicks] = useState(0);
  const gallery: FilePreviewItem[] = markdownImageSources(
    props.imageGroupMarkdown ?? props.markdown,
  )
    .map((href) => (changeRequestUrl ? resolveChangeRequestMediaUrl(href, changeRequestUrl) : href))
    .filter(
      (uri) =>
        /^https?:\/\//i.test(uri) &&
        videoMimeType({ name: uri.split(/[?#]/)[0] ?? uri, mimeType: "" }) === null,
    )
    .map((uri) => pullRequestImageSource(uri, mediaContext));
  const segmentProps = {
    gallery,
    lineBreaks: props.lineBreaks,
    undoneTicks,
    onTaskListChange: props.onTaskListChange,
    onTaskListUndone: () => setUndoneTicks((count) => count + 1),
  };
  return (
    <View style={{ gap: 12 }}>
      {segments.map((segment) => (
        <PullRequestBodySegmentView key={segment.id} segment={segment} {...segmentProps} />
      ))}
    </View>
  );
}

function PullRequestBodySegmentView(props: {
  readonly segment: PullRequestBodySegment;
  readonly gallery: FilePreviewItem[];
  readonly lineBreaks: boolean | undefined;
  readonly undoneTicks: number;
  readonly onTaskListChange:
    | ((input: { readonly markerOffset: number; readonly checked: boolean }) => boolean)
    | undefined;
  readonly onTaskListUndone: () => void;
}) {
  const { segment, onTaskListChange } = props;
  const autolinks = useContext(AutolinksContext);
  const githubEmoji = useContext(GithubEmojiContext);
  if (segment.kind === "markdown") {
    const prose = nativePullRequestMarkdown(segment.text, autolinks, githubEmoji);
    return (
      <PullRequestMarkdownContent
        key={props.undoneTicks}
        markdown={prose.markdown}
        gallery={props.gallery}
        lineBreaks={props.lineBreaks}
        onTaskListItemPress={
          onTaskListChange
            ? (event) => {
                const offset = markdownTaskMarkerOffset(
                  prose.markdown,
                  event.index,
                  event.text,
                  event.checked,
                );
                const kept =
                  offset !== null &&
                  onTaskListChange({
                    markerOffset: segment.sourceOffset + prose.sourceOffset(offset),
                    checked: event.checked,
                  });
                if (!kept) props.onTaskListUndone();
              }
            : undefined
        }
      />
    );
  }
  if (segment.kind === "attachment") {
    return (
      <PullRequestMedia
        key={segment.url}
        url={segment.url}
        kind={segment.media}
        {...(segment.label ? { caption: segment.label } : {})}
      />
    );
  }
  return (
    <View style={{ gap: 12 }}>
      {segment.rows.map((cells, rowIndex) => (
        // oxlint-disable-next-line react/no-array-index-key -- rows have no identity besides position
        <View key={rowIndex} style={{ flexDirection: "row", gap: 12 }}>
          {cells.map((cell, cellIndex) => (
            // oxlint-disable-next-line react/no-array-index-key -- cells have no identity besides position
            <View key={cellIndex} style={{ flex: 1, minWidth: 0, gap: 12 }}>
              {cell.segments.map((cellSegment) => (
                <PullRequestBodySegmentView key={cellSegment.id} {...props} segment={cellSegment} />
              ))}
            </View>
          ))}
        </View>
      ))}
    </View>
  );
}

function PullRequestMarkdownContent(props: {
  readonly markdown: string;
  readonly gallery: FilePreviewItem[];
  readonly lineBreaks: boolean | undefined;
  readonly onTaskListItemPress?: SelectableMarkdownTextProps["onTaskListItemPress"];
}) {
  const hostLineBreaks = useContext(LineBreaksContext);
  const mediaContext = useContext(PullRequestMediaContext);
  const changeRequestUrl = useContext(ChangeRequestUrlContext);
  const resolveMediaUrl = (href: string) =>
    changeRequestUrl ? resolveChangeRequestMediaUrl(href, changeRequestUrl) : href;
  const openPullRequest = useOpenPullRequest();
  const fileLinkCheckout = useContext(FileLinkCheckoutContext);
  const fileBase = useContext(FileBaseContext);
  const navigation = useNavigation();
  const openLink = (href: string) => {
    const file = pullRequestFileLinkRoute(href, fileLinkCheckout);
    if (file === null) void openPullRequest(resolveChangeRequestFileHref(href, fileBase) ?? href);
    else navigation.dispatch(StackActions.push("NewTaskFile", file));
  };
  const [preview, setPreview] = useState<FilePreviewSource | null>(null);
  const renderImage: MarkdownImageRenderer = (image) => (
    <MarkdownImage href={resolveMediaUrl(image.href)} alt={image.alt} onPreview={setPreview} />
  );
  const styles = useMarkdownPreviewStyles();
  return (
    <FilePreviewGallery value={props.gallery}>
      <SelectableMarkdownText
        markdown={props.markdown}
        preserveSoftBreaks={props.lineBreaks ?? hostLineBreaks}
        textStyle={styles.nativeTextStyle}
        onLinkPress={openLink}
        renderImage={renderImage}
        onImagePress={(href) =>
          setPreview(pullRequestImageSource(resolveMediaUrl(href), mediaContext))
        }
        onTaskListItemPress={props.onTaskListItemPress}
      />
      <FilePreviewModal source={preview} onRequestClose={() => setPreview(null)} />
    </FilePreviewGallery>
  );
}
