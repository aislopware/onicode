// The updater of the earlier native Onicode runs a staged bundle's `--version` before installing
// it, from any directory, and wants exactly `onicode <version>`, so answer before anything else loads.
if (process.argv.includes("--version")) {
  const { app } = require("electron");
  try {
    require("node:fs").writeSync(1, `onicode ${app.getVersion()}\n`);
  } catch {}
  app.exit(0);
} else {
  require("./compileCache.cjs");
  require("./main.cjs");
}
