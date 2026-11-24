import { requireNativeModule } from "expo";
import { msg } from "gt-react-native";
import { Platform } from "react-native";

import { m } from "../i18n/messages";

const SAVABLE_IMAGE_MIME_TYPE = /^image\/(png|jpe?g|gif|webp|heic|heif)$/i;

function baseMimeType(mimeType: string): string {
  return mimeType.split(";", 1)[0]?.trim() ?? "";
}

// Android 9 and older need a storage permission to add photos, so they keep the share sheet.
export function canSaveImageToLibrary(mimeType: string): boolean {
  return (
    SAVABLE_IMAGE_MIME_TYPE.test(baseMimeType(mimeType)) &&
    (Platform.OS === "ios" || (Platform.OS === "android" && Platform.Version >= 29))
  );
}

/** Adds a local image file to Photos on iOS or Pictures/Onicode on Android. */
export async function saveImageToLibrary(
  uri: string,
  mimeType: string,
  name: string,
): Promise<void> {
  try {
    await requireNativeModule<{
      saveImageToLibrary(uri: string, mimeType: string, name: string): Promise<void>;
    }>("OniNativeControls").saveImageToLibrary(uri, baseMimeType(mimeType), name);
  } catch (error) {
    const message = saveErrorMessage(error);
    throw message === null ? error : new Error(message, { cause: error });
  }
}

/** The native error's text in the UI language, or null for system errors to show as they are. */
function saveErrorMessage(error: unknown): string | null {
  const code = typeof error === "object" && error !== null && "code" in error ? error.code : null;
  switch (code) {
    case "ERR_PHOTOS_NOT_ALLOWED":
      return m(msg("Allow {app} to add photos in Settings.", { app: "Onicode" }));
    case "ERR_PHOTOS_SAVE_FAILED":
      return m(msg("Photos could not save this image."));
    case "ERR_IMAGE_MISSING":
      return m(msg("The image is no longer available."));
    case "ERR_PICTURES_SAVE_FAILED":
      return m(msg("The image could not be saved to Pictures."));
    default:
      return null;
  }
}
