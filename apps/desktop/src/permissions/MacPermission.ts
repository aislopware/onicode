export const MAC_PERMISSION_SETTINGS_URLS = {
  "screen-recording":
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
  accessibility: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
  "full-disk-access":
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
  "developer-tools": "x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools",
};

export type MacPermission = keyof typeof MAC_PERMISSION_SETTINGS_URLS;

export const MAC_PERMISSION_TITLES: Record<MacPermission, string> = {
  "screen-recording": "Screen Recording",
  accessibility: "Accessibility",
  "full-disk-access": "Full Disk Access",
  "developer-tools": "Developer Tools",
};
