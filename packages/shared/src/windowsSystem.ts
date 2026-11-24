// Effect-free so the standalone service launcher can use it.

/**
 * A Windows variable in any case. A copied environment keeps the spelling of its names; only the
 * live one ignores case, and MSYS and Cygwin launch with upper-case names such as `SYSTEMROOT`.
 */
export const windowsEnvironmentValue = (
  env: Readonly<Record<string, string | undefined>>,
  name: string,
): string | undefined => {
  if (env[name] !== undefined) return env[name];
  const wanted = name.toUpperCase();
  return Object.entries(env).find(
    ([candidate, value]) => value !== undefined && candidate.toUpperCase() === wanted,
  )?.[1];
};

export const windowsRoot = (env: Readonly<Record<string, string | undefined>>) =>
  windowsEnvironmentValue(env, "SystemRoot") ||
  windowsEnvironmentValue(env, "windir") ||
  String.raw`C:\Windows`;

/** Windows system tools are run by absolute path, independent of PATH and the working directory. */
export const windowsSystem32Path = (
  env: Readonly<Record<string, string | undefined>>,
  executable: string,
) => `${windowsRoot(env)}\\System32\\${executable}`;

export const windowsPowerShellPath = (env: Readonly<Record<string, string | undefined>>) =>
  windowsSystem32Path(env, String.raw`WindowsPowerShell\v1.0\powershell.exe`);
