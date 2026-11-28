import { useGT } from "gt-react-native";
import { SymbolView } from "./ui/AppSymbol";
import { AppText } from "./ui/AppText";
import { Image } from "expo-image";
import { memo, useLayoutEffect, useMemo, useRef, useState } from "react";
import { View } from "react-native";
import type { EnvironmentProject } from "@onicode/client-runtime/state/models";
import type { EnvironmentId, ProjectIconOverride } from "@onicode/contracts";
import {
  getProjectFaviconCacheKey,
  getProjectFaviconResourceKey,
  isProjectFaviconFallbackUrl,
} from "@onicode/shared/projectFavicon";
import { useAtomValue } from "@effect/atom-react";
import { Atom } from "effect/reactivity";
import { useAppearancePreferences } from "../features/settings/appearance/AppearancePreferencesProvider";
import { projectFaviconUrlAtom, refreshProjectFaviconUrl } from "../state/assets";
import {
  countGlyphs,
  projectAutoMonogram,
  projectIconColorClassNames,
  resolveProjectIconGlyph,
  resolveProjectIconSymbol,
  type ProjectIconGlyph,
} from "../lib/projectIcon";
import { resolveProjectFaviconColorScheme } from "../lib/projectFaviconColorScheme";

import {
  beginProjectFaviconRequest,
  createProjectFaviconRequest,
  hasLoadedProjectFavicon,
  markProjectFaviconFailed,
  markProjectFaviconLoaded,
} from "../lib/projectFaviconRequests";

const EMPTY_FAVICON_URL = Atom.make<string | null>(null);

export const ProjectFavicon = memo(function ProjectFavicon(props: {
  readonly environmentId: EnvironmentId;
  readonly open?: boolean;
  readonly size?: number;
  readonly projectTitle: string;
  readonly workspaceRoot?: string | null;
  readonly faviconPath?: string | null;
  readonly projectIcon?: ProjectIconOverride | null;
  /** Projects whose icon to show, in order, when this project has none. */
  readonly fallbackProjects?: ReadonlyArray<ProjectFaviconSource>;
}) {
  const size = props.size ?? 42;
  const symbol = resolveProjectIconSymbol(props.projectIcon);
  const glyph = resolveProjectIconGlyph(props.projectIcon, props.projectTitle);
  const faviconTarget =
    props.workspaceRoot == null || glyph !== null
      ? null
      : {
          environmentId: props.environmentId,
          cwd: props.workspaceRoot,
          faviconPath: props.faviconPath,
        };
  const faviconUrl = useAtomValue(
    faviconTarget === null ? EMPTY_FAVICON_URL : projectFaviconUrlAtom(faviconTarget),
  );
  const renderableFaviconUrl = isProjectFaviconFallbackUrl(faviconUrl) ? null : faviconUrl;
  // Key remote images by revision so token rotation reuses the cache while changed icons reload.
  const cacheKey =
    renderableFaviconUrl && props.workspaceRoot
      ? renderableFaviconUrl.startsWith("data:")
        ? getProjectFaviconResourceKey(props.environmentId, props.workspaceRoot, props.faviconPath)
        : getProjectFaviconCacheKey(props.environmentId, props.workspaceRoot, renderableFaviconUrl)
      : null;

  if (symbol !== null) {
    return (
      <SymbolView
        name={symbol.name}
        size={size}
        tintColorClassName={projectIconColorClassNames(symbol.color).tint}
        type="monochrome"
      />
    );
  }

  if (glyph !== null) {
    return <ProjectIconGlyphView glyph={glyph} size={size} />;
  }

  if (renderableFaviconUrl === null && props.fallbackProjects?.length) {
    return (
      <FallbackProjectFavicon
        projects={props.fallbackProjects}
        projectTitle={props.projectTitle}
        size={size}
        open={props.open}
      />
    );
  }

  return (
    <ProjectFaviconImage
      key={cacheKey}
      cacheKey={cacheKey}
      faviconUrl={renderableFaviconUrl}
      open={props.open}
      projectTitle={props.projectTitle}
      size={size}
      // An icon too large to cache inline loads from a signed URL that expires.
      onRemoteLoadError={
        faviconTarget === null ? undefined : () => refreshProjectFaviconUrl(faviconTarget)
      }
    />
  );
});

type ProjectFaviconSource = Pick<
  EnvironmentProject,
  "environmentId" | "workspaceRoot" | "faviconPath" | "projectIcon"
>;

function FallbackProjectFavicon(props: {
  readonly projects: ReadonlyArray<ProjectFaviconSource>;
  readonly projectTitle: string;
  readonly size: number;
  readonly open: boolean | undefined;
}) {
  const [project, ...laterProjects] = props.projects;
  if (!project) return null;
  return (
    <ProjectFavicon
      environmentId={project.environmentId}
      workspaceRoot={project.workspaceRoot}
      faviconPath={project.faviconPath}
      projectIcon={project.projectIcon}
      projectTitle={props.projectTitle}
      size={props.size}
      fallbackProjects={laterProjects}
      {...(props.open === undefined ? {} : { open: props.open })}
    />
  );
}

function ProjectIconGlyphView(props: { readonly glyph: ProjectIconGlyph; readonly size: number }) {
  const { glyph, size } = props;
  if (glyph.kind === "emoji") {
    return (
      <View style={{ width: size, height: size, alignItems: "center", justifyContent: "center" }}>
        <AppText
          allowFontScaling={false}
          style={{
            fontSize: size * 0.8,
            lineHeight: size,
            textAlign: "center",
            includeFontPadding: false,
          }}
        >
          {glyph.emoji}
        </AppText>
      </View>
    );
  }

  const colors = projectIconColorClassNames(glyph.color);
  return (
    <View
      className={colors.background}
      style={{
        width: size,
        height: size,
        borderRadius: size * 0.25,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <AppText
        allowFontScaling={false}
        numberOfLines={1}
        className={`font-mono ${colors.text}`}
        style={{
          fontWeight: "700",
          fontSize: size * (countGlyphs(glyph.text) === 1 ? 0.6 : 0.515625),
          lineHeight: size,
          textAlign: "center",
          includeFontPadding: false,
        }}
      >
        {glyph.text}
      </AppText>
    </View>
  );
}

function ProjectFaviconImage(props: {
  readonly cacheKey: string | null;
  readonly faviconUrl: string | null;
  readonly open?: boolean;
  readonly projectTitle: string;
  readonly size: number;
  readonly onRemoteLoadError?: () => void;
}) {
  const gt = useGT();
  const faviconRequest = createProjectFaviconRequest(props.cacheKey, props.faviconUrl);
  // Registers with the module-level loader before the Image's native onLoad/onError can fire,
  // so stale loads for a superseded URL under the same cacheKey are ignored.
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- marks this favicon URL as the current request in the module registry before the native Image reports load or error
  useLayoutEffect(() => {
    if (faviconRequest === null) return;
    return beginProjectFaviconRequest(faviconRequest);
  }, [faviconRequest]);

  const [status, setStatus] = useState<"loading" | "loaded" | "error">(() =>
    props.faviconUrl?.startsWith("data:") || hasLoadedProjectFavicon(props.cacheKey)
      ? "loaded"
      : "loading",
  );
  // One refresh per failure: a fresh URL for a file that cannot load would fail again.
  const refreshedAfterErrorRef = useRef(false);

  const { themeAppearance } = useAppearancePreferences();
  const requestFaviconUrl = faviconRequest?.faviconUrl;
  const inlineFaviconUrl = useMemo(
    () =>
      requestFaviconUrl?.startsWith("data:")
        ? resolveProjectFaviconColorScheme(requestFaviconUrl, themeAppearance)
        : null,
    [requestFaviconUrl, themeAppearance],
  );

  const showImage = faviconRequest !== null && status === "loaded";
  const fallbackGlyph = showImage ? null : projectAutoMonogram(props.projectTitle);

  return (
    <View
      style={{
        width: props.size,
        height: props.size,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {showImage ? null : fallbackGlyph ? (
        <ProjectIconGlyphView glyph={fallbackGlyph} size={props.size} />
      ) : (
        <SymbolView
          name={{ ios: "folder.fill", android: props.open ? "folder_open" : "folder" }}
          size={props.size}
          tintColorClassName={"accent-icon-subtle"}
          type="monochrome"
        />
      )}

      {faviconRequest !== null ? (
        <Image
          key={faviconRequest.faviconUrl}
          source={
            inlineFaviconUrl !== null
              ? { uri: inlineFaviconUrl }
              : { uri: faviconRequest.faviconUrl, cacheKey: faviconRequest.cacheKey }
          }
          cachePolicy={inlineFaviconUrl !== null ? "memory" : "memory-disk"}
          recyclingKey={faviconRequest.cacheKey}
          accessibilityLabel={gt("{projectTitle} favicon", { projectTitle: props.projectTitle })}
          style={{
            width: props.size,
            height: props.size,
            borderRadius: props.size * 0.16,
            ...(showImage ? {} : { position: "absolute" as const, opacity: 0 }),
          }}
          contentFit="contain"
          onLoad={() => {
            if (!markProjectFaviconLoaded(faviconRequest)) return;
            refreshedAfterErrorRef.current = false;
            setStatus("loaded");
          }}
          onError={() => {
            if (!markProjectFaviconFailed(faviconRequest)) return;
            setStatus("error");
            if (faviconRequest.faviconUrl.startsWith("data:") || refreshedAfterErrorRef.current)
              return;
            refreshedAfterErrorRef.current = true;
            props.onRemoteLoadError?.();
          }}
        />
      ) : null}
    </View>
  );
}
