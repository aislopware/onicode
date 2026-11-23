import type { TextInputWrapperViewProps } from "expo-paste-input";
import { useEffect, useRef } from "react";

type LandPastedImages = (uris: ReadonlyArray<string>) => void;

/**
 * Props for `TextInputWrapper`. iOS reads a pasted image off the main thread, so a tap can come
 * before its files: `startPaste` runs when the read starts, to hold that tap, and the callback it
 * returns gets the files, none when nothing loaded. Without a read start it runs on the files.
 * `images` is how many images the paste carries.
 */
export function useNativePaste(
  startPaste: (images: number) => LandPastedImages | undefined,
): Pick<TextInputWrapperViewProps, "onPaste" | "onImageReadStart"> {
  const reads = useRef(new Map<number, LandPastedImages>());
  // oxlint-disable-next-line onicode/no-unreasoned-state-sync -- an unmounted input gets no paste event, so its started reads release their holds here
  useEffect(() => {
    const pending = reads.current;
    return () => {
      const lands = [...pending.values()];
      pending.clear();
      for (const land of lands) land([]);
    };
  }, []);
  return {
    // Native builds before the count was added announce no `images`.
    onImageReadStart: ({ readId, images = 1 }) => {
      const land = startPaste(images);
      if (land) reads.current.set(readId, land);
    },
    onPaste: (payload) => {
      if (payload.type === "text") return;
      const land = payload.readId === undefined ? undefined : reads.current.get(payload.readId);
      if (payload.readId !== undefined) reads.current.delete(payload.readId);
      const uris = payload.type === "images" ? payload.uris : [];
      if (land) land(uris);
      else if (uris.length > 0) startPaste(uris.length)?.(uris);
    },
  };
}
