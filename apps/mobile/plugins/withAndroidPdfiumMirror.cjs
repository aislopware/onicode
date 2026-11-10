const { withGradleProperties } = require("expo/config-plugins");

// react-native-pdf-jsi 6.1.1 downloads PDFium from a release upstream never published
// (126punith/react-native-pdf-jsi#46). The mirror holds the same zip; Gradle still checks its SHA-256.
const PDFIUM_URL =
  "https://github.com/aislopware/react-native-pdf-jsi/releases/download/pdfium-android-r1/pdfium-android-r1.zip";

module.exports = function withAndroidPdfiumMirror(config) {
  return withGradleProperties(config, (nextConfig) => {
    nextConfig.modResults = nextConfig.modResults.filter(
      (item) => !(item.type === "property" && item.key === "pdfJsiPdfiumUrl"),
    );
    nextConfig.modResults.push({ type: "property", key: "pdfJsiPdfiumUrl", value: PDFIUM_URL });
    return nextConfig;
  });
};
