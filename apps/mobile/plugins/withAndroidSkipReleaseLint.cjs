const { withAppBuildGradle } = require("expo/config-plugins");

// lintVital lints every module on each release build, minutes that no cache saves; CI's own checks
// cover the app.
const BLOCK = "android { lint { checkReleaseBuilds = false } }";

module.exports = function withAndroidSkipReleaseLint(config) {
  return withAppBuildGradle(config, (nextConfig) => {
    if (!nextConfig.modResults.contents.includes(BLOCK))
      nextConfig.modResults.contents += `\n${BLOCK}\n`;
    return nextConfig;
  });
};
