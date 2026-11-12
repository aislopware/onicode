// @effect-diagnostics nodeBuiltinImport:off - Build bootstrap reads optional root env files before an Effect runtime exists.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";
import * as NodeURL from "node:url";
import * as NodeUtil from "node:util";

type Environment = Readonly<Record<string, string | undefined>>;

const REPO_ROOT = NodePath.resolve(NodeURL.fileURLToPath(import.meta.url), "../../..");

/** The root `.env`, then `.env.local`, then the process environment, later ones winning. */
export function loadRepoEnv({
  baseEnv = process.env,
  repoRoot = REPO_ROOT,
}: {
  readonly baseEnv?: Environment;
  readonly repoRoot?: string;
} = {}): Record<string, string | undefined> {
  return {
    ...readEnvFile(NodePath.join(repoRoot, ".env")),
    ...readEnvFile(NodePath.join(repoRoot, ".env.local")),
    ...baseEnv,
  };
}

function readEnvFile(path: string): Record<string, string | undefined> {
  return NodeFS.existsSync(path) ? NodeUtil.parseEnv(NodeFS.readFileSync(path, "utf8")) : {};
}

const MAINTAINER_APP_ID = "dev.aislopware.onicode";
// Valid as both an iOS bundle identifier and an Android package name.
const APP_ID_PATTERN = /^[A-Za-z][A-Za-z0-9]*(?:\.[A-Za-z][A-Za-z0-9]*)+$/;

/**
 * Whose mobile app a build is. ONICODE_MOBILE_APP_ID makes it yours, and then the maintainers'
 * Apple team is not inherited. No push gateway or Firebase project is built in for anyone: both
 * serve only the builds signed for them.
 */
export function resolveMobileIdentity(env: Environment) {
  const setting = (name: string) => env[name]?.trim() || undefined;
  const appId = setting("ONICODE_MOBILE_APP_ID") ?? MAINTAINER_APP_ID;
  if (!APP_ID_PATTERN.test(appId)) {
    throw new Error(
      "ONICODE_MOBILE_APP_ID must be a reverse-DNS identifier of letters and digits, such as com.example.onicode.",
    );
  }
  const own = appId !== MAINTAINER_APP_ID;
  return {
    own,
    appId,
    appleTeamId: setting("ONICODE_APPLE_TEAM_ID") ?? (own ? undefined : "UK58J62H8L"),
    /** Relative paths resolve from apps/mobile. */
    androidGoogleServicesFile: setting("ONICODE_ANDROID_GOOGLE_SERVICES_FILE"),
    pushGatewayUrl: setting("ONICODE_PUSH_GATEWAY_URL"),
  };
}
