// @effect-diagnostics nodeBuiltinImport:off -- This native boundary asks TCC through osascript, which has no Node binding.
import * as NodeChildProcess from "node:child_process";

// TCC answers for the responsible app: Onicode when packaged, whatever launched a dev build.
// 0 is granted and 1 denied; anything else means the private API changed.
const PREFLIGHT_SCRIPT = `
ObjC.import("Foundation");
function run() {
  $.NSBundle.bundleWithPath("/System/Library/PrivateFrameworks/TCC.framework").load;
  ObjC.bindFunction("TCCAccessPreflight", ["int", ["id", "id"]]);
  return String($.TCCAccessPreflight($("kTCCServiceDeveloperTool"), $()));
}`;

export function developerToolsGranted(): Promise<boolean> {
  return new Promise((resolve, reject) => {
    NodeChildProcess.execFile(
      "/usr/bin/osascript",
      ["-l", "JavaScript", "-e", PREFLIGHT_SCRIPT],
      { timeout: 5_000 },
      (error, stdout) => {
        const result = stdout.trim();
        if (error || (result !== "0" && result !== "1")) {
          reject(error ?? new Error(`Unexpected Developer Tools preflight result: ${result}`));
        } else {
          resolve(result === "0");
        }
      },
    );
  });
}
