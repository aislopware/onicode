import * as NodeFSP from "node:fs/promises";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import { build } from "vite-plus";

const mobileRoot = NodePath.resolve(NodePath.dirname(NodeURL.fileURLToPath(import.meta.url)), "..");

// Each document calls its script's global; the test keeps these names matched to theirs.
const STREAM_SCRIPTS = {
  device: { entry: "devices/device-stream.browser.ts", global: "OniDeviceStream" },
  preview: { entry: "browser/preview-stream.browser.ts", global: "OniPreviewStream" },
  "remote-desktop": {
    entry: "remote-desktop/remote-desktop.browser.ts",
    global: "OniRemoteDesktop",
  },
} as const;

export async function buildWebViewScript(stream: keyof typeof STREAM_SCRIPTS) {
  const { entry, global } = STREAM_SCRIPTS[stream];
  const result = await build({
    configFile: false,
    logLevel: "silent",
    build: {
      write: false,
      target: "es2022",
      minify: true,
      lib: {
        entry: NodePath.join(mobileRoot, "src/features", entry),
        name: global,
        formats: ["iife"],
      },
    },
  });
  const bundles = Array.isArray(result) ? result : [result];
  const chunk = bundles
    .flatMap((bundle) => ("output" in bundle ? bundle.output : []))
    .find((output) => output.type === "chunk");
  if (!chunk) throw new Error(`${global} build did not emit a script.`);
  return chunk.code;
}

async function generateWebViewScript(stream: keyof typeof STREAM_SCRIPTS) {
  const code = await buildWebViewScript(stream);
  const root = NodePath.join(mobileRoot, ".generated", `${stream}-stream`);
  await NodeFSP.mkdir(root, { recursive: true });
  for (const [file, contents] of [
    ["index.js", `module.exports = ${JSON.stringify(code)};\n`],
    ["package.json", '{"main":"index.js"}\n'],
  ] as const) {
    const destination = NodePath.join(root, file);
    const previous = await NodeFSP.readFile(destination, "utf8").catch(() => null);
    if (previous !== contents) await NodeFSP.writeFile(destination, contents);
  }
}

export const generateDeviceStreamScript = () => generateWebViewScript("device");
export const generatePreviewStreamScript = () => generateWebViewScript("preview");
export const generateRemoteDesktopScript = () => generateWebViewScript("remote-desktop");
