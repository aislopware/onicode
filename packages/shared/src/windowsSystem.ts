// Effect-free so the standalone service launcher can use it.

/** A copied environment keeps the spelling of its names; only the live one ignores case. */
export const windowsRoot = (env: Readonly<Record<string, string | undefined>>) =>
  env.SystemRoot || env.SYSTEMROOT || env.windir || env.WINDIR || String.raw`C:\Windows`;

/** Windows system tools are run by absolute path, independent of PATH and the working directory. */
export const windowsSystem32Path = (
  env: Readonly<Record<string, string | undefined>>,
  executable: string,
) => `${windowsRoot(env)}\\System32\\${executable}`;

export const windowsPowerShellPath = (env: Readonly<Record<string, string | undefined>>) =>
  windowsSystem32Path(env, String.raw`WindowsPowerShell\v1.0\powershell.exe`);
