// @ts-check
// The fingerprint is the production runtime version: it decides whether a release needs a store
// build or can ship as an update. Expo drops this whole file without a word when it throws.
const majorVersion = require("../server/package.json").version.split(".")[0];

module.exports = {
  // pnpm patches change native modules and config plugins without changing their versions.
  nativeModuleSourceType: "files",
  configPluginSourceType: "files",
  // Expo's default skips, plus package.json scripts and .gitignore: the native projects are
  // generated on every build, so neither reaches one.
  sourceSkips: [
    "ExpoConfigVersions",
    "ExpoConfigRuntimeVersionIfString",
    "ExpoConfigExtraSection",
    "EasJson",
    "Easignore",
    "AutolinkingConfigPaths",
    "PackageJsonScriptsAll",
    "GitIgnore",
  ],
  // Gradle builds Sentry's nested android/expo-handler project in place, and Expo only ignores
  // android/build, so the first build would change the hash of its own inputs. Kept this narrow:
  // Sentry ships other build output in its package, and hashing that keeps runtime versions put.
  ignorePaths: ["**/android/expo-handler/{build,.cxx,.gradle}/**/*"],
  // Expo ignores the app version; the major keeps updates off older store binaries.
  extraSources: [
    { type: "contents", id: "appMajorVersion", contents: majorVersion },
    // Expo skips symlinks when hashing local modules.
    {
      type: "file",
      filePath: "../../native/apple-speech/AppleSpeechEngine.swift",
      reasons: ["appleSpeech"],
    },
  ],
};
