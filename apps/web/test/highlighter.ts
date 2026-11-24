import {
  DEFAULT_THEMES,
  disposeHighlighter,
  preloadHighlighter,
  type SupportedLanguages,
} from "@pierre/diffs";

// The shared highlighter is module state; one left loaded changes how later tests in the file render.
export async function loadSharedHighlighter(langs: SupportedLanguages[]): Promise<AsyncDisposable> {
  await preloadHighlighter({ themes: Object.values(DEFAULT_THEMES), langs });
  return { [Symbol.asyncDispose]: disposeHighlighter };
}
