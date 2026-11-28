import * as NodeServices from "@effect/platform-node/NodeServices";
import { describe, expect, it } from "@effect/vitest";
import { SecretProviderId } from "@onicode/contracts";
import * as Effect from "effect/Effect";
import * as Path from "effect/Path";
import * as HostProcess from "@onicode/shared/HostProcess";

import { environmentView, mergeProviderInstanceEnvironment } from "./instanceEnvironment.ts";

describe("mergeProviderInstanceEnvironment", () => {
  it.effect.each([
    { value: "~/.account", tail: ".account" },
    { value: "~\\.account\\work", tail: ".account\\work" },
  ])("expands configured provider homes set to $value", ({ value, tail }) =>
    Effect.gen(function* () {
      const path = yield* Path.Path;
      const baseEnv = {
        CODEX_HOME: "~/.inherited-codex",
        CLAUDE_CONFIG_DIR: "~/.inherited-claude",
        GROK_HOME: "~/.inherited-grok",
      };
      const environment = yield* mergeProviderInstanceEnvironment(
        [
          { name: "CODEX_HOME", value, sensitive: false },
          { name: "CLAUDE_CONFIG_DIR", value, sensitive: false },
          { name: "GROK_HOME", value, sensitive: false },
          { name: "CUSTOM_VALUE", value, sensitive: false },
        ],
        baseEnv,
      );

      expect(environment).toEqual({
        CODEX_HOME: path.join("/home/ada", tail),
        CLAUDE_CONFIG_DIR: path.join("/home/ada", tail),
        GROK_HOME: path.join("/home/ada", tail),
        CUSTOM_VALUE: value,
      });
      expect(baseEnv).toEqual({
        CODEX_HOME: "~/.inherited-codex",
        CLAUDE_CONFIG_DIR: "~/.inherited-claude",
        GROK_HOME: "~/.inherited-grok",
      });
    }).pipe(
      Effect.provideService(HostProcess.HomeDirectory, "/home/ada"),
      Effect.provide(NodeServices.layer),
    ),
  );

  it.effect("leaves inherited provider homes unchanged", () =>
    Effect.gen(function* () {
      const baseEnv = { CODEX_HOME: "~/.codex", CLAUDE_CONFIG_DIR: "~\\.claude" };

      expect(
        yield* mergeProviderInstanceEnvironment(
          [{ name: "CUSTOM_VALUE", value: "~/.custom", sensitive: false }],
          baseEnv,
        ),
      ).toEqual({ ...baseEnv, CUSTOM_VALUE: "~/.custom" });
    }),
  );

  it.effect("keeps reading host variables it does not set", () =>
    Effect.gen(function* () {
      const host: NodeJS.ProcessEnv = { PATH: "/bin" };
      const environment = yield* mergeProviderInstanceEnvironment(
        [{ name: "CUSTOM_VALUE", value: "1", sensitive: false }],
        host,
      );
      // The desktop's shell snapshot reaches the host after drivers are built.
      host.PATH = "/opt/bin:/bin";
      host.NODE_EXTRA_CA_CERTS = "/etc/ca.pem";
      expect({ ...environment }).toEqual({
        PATH: "/opt/bin:/bin",
        NODE_EXTRA_CA_CERTS: "/etc/ca.pem",
        CUSTOM_VALUE: "1",
      });
      expect(host).toEqual({ PATH: "/opt/bin:/bin", NODE_EXTRA_CA_CERTS: "/etc/ca.pem" });
    }),
  );

  it.effect("keeps reading an AppImage host's variables, without the mount's paths", () =>
    Effect.gen(function* () {
      const host: NodeJS.ProcessEnv = { APPDIR: "/tmp/.mount_Onicod", PATH: "/bin" };
      const environment = yield* mergeProviderInstanceEnvironment(undefined, host);
      host.PATH = "/tmp/.mount_Onicod/usr/bin:/opt/bin:/bin";
      expect({ ...environment }).toEqual({ PATH: "/opt/bin:/bin" });
    }),
  );

  it.effect("overrides inherited environment values and preserves empty strings", () =>
    Effect.gen(function* () {
      expect(
        yield* mergeProviderInstanceEnvironment(
          [
            { name: "OPENROUTER_API_KEY", value: "sk-or-test", sensitive: true },
            { name: "ANTHROPIC_API_KEY", value: "", sensitive: false },
            {
              name: "OPENAI_API_KEY",
              value: {
                provider: SecretProviderId.make("personal"),
                reference: "op://Private/OpenAI/credential",
              },
              sensitive: true,
            },
          ],
          { ANTHROPIC_API_KEY: "inherited", OPENAI_API_KEY: "inherited", PATH: "/bin" },
        ),
      ).toMatchObject({
        OPENROUTER_API_KEY: "sk-or-test",
        ANTHROPIC_API_KEY: "",
        OPENAI_API_KEY: "inherited",
        PATH: "/bin",
      });
    }),
  );

  const appDir = "/tmp/.mount_Onicod-XXXX";
  const systemLibraryPath = "/home/ada/lib:/home/ada/.local/lib";
  it.effect.each([
    {
      label: "without configured variables",
      environment: undefined,
      libraryPath: `${appDir}/usr/lib;${systemLibraryPath}`,
      keptLibraryPath: systemLibraryPath,
    },
    {
      label: "alongside configured variables",
      environment: [{ name: "CUSTOM_VALUE", value: "set", sensitive: false }],
      libraryPath: `${appDir}/usr/lib;${systemLibraryPath}`,
      keptLibraryPath: systemLibraryPath,
    },
    {
      label: "keeping a lone current-directory entry",
      environment: undefined,
      libraryPath: `${appDir}/usr/lib:`,
      keptLibraryPath: ":",
    },
    {
      label: "when mount entries contain redundant separators or dot segments",
      environment: undefined,
      libraryPath: `/tmp/./.mount_Onicod-XXXX//usr/./lib;/tmp/link/../.mount_Onicod-XXXX/lib;${appDir}/../../opt/lib`,
      keptLibraryPath: `/tmp/link/../.mount_Onicod-XXXX/lib:${appDir}/../../opt/lib`,
    },
  ])(
    "drops the AppImage runtime from the inherited environment $label",
    ({ environment, libraryPath, keptLibraryPath }) =>
      Effect.gen(function* () {
        const merged = yield* mergeProviderInstanceEnvironment(environment, {
          APPIMAGE: "/home/ada/Onicode.AppImage",
          APPDIR: appDir,
          ARGV0: "/home/ada/Onicode.AppImage",
          OWD: "/home/ada",
          PATH: `${appDir}:${appDir}/usr/sbin::/usr/bin`,
          LD_LIBRARY_PATH: libraryPath,
          XDG_DATA_DIRS: `${appDir}/usr/share/:/usr/share`,
          GSETTINGS_SCHEMA_DIR: `${appDir}/usr/share/glib-2.0/schemas`,
        });

        expect({ ...merged }).toEqual({
          PATH: ":/usr/bin",
          LD_LIBRARY_PATH: keptLibraryPath,
          XDG_DATA_DIRS: "/usr/share",
          ...(environment ? { CUSTOM_VALUE: "set" } : {}),
        });
      }),
  );

  it.effect("replaces every inherited spelling of a name on Windows, and only there", () =>
    Effect.gen(function* () {
      const merge = (platform: NodeJS.Platform) =>
        mergeProviderInstanceEnvironment(
          [{ name: "Https_Proxy", value: "http://gateway", sensitive: false }],
          { HTTPS_PROXY: "http://mine", PATH: "/bin" },
        ).pipe(Effect.provideService(HostProcess.Platform, platform));
      // A home variable in another spelling is still expanded.
      expect(
        yield* mergeProviderInstanceEnvironment(
          [{ name: "Codex_Home", value: "~/corp", sensitive: false }],
          {},
        ).pipe(
          Effect.provideService(HostProcess.Platform, "win32"),
          Effect.provideService(HostProcess.HomeDirectory, "/home/ada"),
        ),
      ).toEqual({ CODEX_HOME: "/home/ada/corp" });
      // Upper case sorts first, so it also wins where a spawn merges the parent environment back.
      expect(yield* merge("win32")).toEqual({ HTTPS_PROXY: "http://gateway", PATH: "/bin" });
      expect(yield* merge("linux")).toEqual({
        HTTPS_PROXY: "http://mine",
        Https_Proxy: "http://gateway",
        PATH: "/bin",
      });
    }),
  );

  it("replaces every host spelling of a name a view writes or deletes on Windows", () => {
    const host: NodeJS.ProcessEnv = { Path: "C:\\bin", Temp: "C:\\tmp" };
    const view = environmentView(host, "win32");
    view.PATH = "C:\\tools";
    delete view.TEMP;
    Object.defineProperty(view, "Extra", {
      value: "1",
      enumerable: true,
      configurable: true,
      writable: true,
    });
    expect({ ...view }).toEqual({ PATH: "C:\\tools", Extra: "1" });
    expect([view.Path, view.extra, "temp" in view]).toEqual(["C:\\tools", "1", false]);
    expect([view.toString, "toString" in view, Object.hasOwn(view, "constructor")]).toEqual([
      undefined,
      false,
      false,
    ]);
    expect(host).toEqual({ Path: "C:\\bin", Temp: "C:\\tmp" });
  });
});
