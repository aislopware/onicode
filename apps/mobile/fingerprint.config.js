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
  // Expo ignores the app version; the major keeps updates off older store binaries.
  extraSources: [{ type: "contents", id: "appMajorVersion", contents: majorVersion }],
};
