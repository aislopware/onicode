import * as NodeUtil from "node:util";

const ACP_STDERR_TAIL_MAX_CHARS = 4_096;
const ACP_STDERR_EXCERPT_MAX_CHARS = 512;

const PAIRING_URL_PATTERN = /https?:\/\/[^\s]*\/pair#[^\s]*/gi;
const SIGN_IN_URL_PATTERN = /https?:\/\/accounts\.google\.com[^\s]*/gi;
const BEARER_TOKEN_PATTERN = /\bBearer\s+[A-Za-z0-9._\-+=/]+/gi;
const BASIC_AUTH_PATTERN = /\bAuthorization:\s*Basic\s+\S+/gi;
const API_KEY_HEADER_PATTERN = /\bx-api-key:\s*\S+/gi;
const SECRET_TOKEN_PATTERN =
  /\b(?:sk-[A-Za-z0-9][A-Za-z0-9_-]{7,}|ghp_[A-Za-z0-9]+|xox[a-zA-Z]-[A-Za-z0-9-]+)\b/g;
// `Error: …`, `TypeError: …`, `Error [ERR_MODULE_NOT_FOUND]: …`, `error: …`, `ERROR: …`, `fatal: …`
const ERROR_LINE_PATTERN = /^(?:[a-z]*error|fatal)(?: ?\[[^\]]+\])?:\s*\S/i;
// glog fatal/error lines emitted by Go/C++ runtimes such as agy (`F1111 11:50:12.383176 ...`).
const GLOG_ERROR_LINE_PATTERN = /^[FE]\d{4}\s+\d{2}:\d{2}:\d{2}(?:\.\d+)?\s+/;
const EXIT_SUMMARY_PATTERN = /\b(?:exit status|exit code|exited with code)\s+\d+\.?$/i;

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}

function isLowSurrogate(code: number): boolean {
  return code >= 0xdc00 && code <= 0xdfff;
}

function boundExcerptHead(value: string): string {
  if (value.length <= ACP_STDERR_EXCERPT_MAX_CHARS) return value;
  let end = ACP_STDERR_EXCERPT_MAX_CHARS - 1;
  if (end > 0 && isHighSurrogate(value.charCodeAt(end - 1))) end -= 1;
  return `${value.slice(0, end)}…`;
}

function boundExcerptTail(value: string): string {
  if (value.length <= ACP_STDERR_EXCERPT_MAX_CHARS) return value;
  let start = value.length - (ACP_STDERR_EXCERPT_MAX_CHARS - 1);
  if (start < value.length && isLowSurrogate(value.charCodeAt(start))) start += 1;
  return `…${value.slice(start)}`;
}

export function appendAcpStderrTail(current: string, chunk: string): string {
  const next = `${current}${chunk}`;
  return next.length <= ACP_STDERR_TAIL_MAX_CHARS ? next : next.slice(-ACP_STDERR_TAIL_MAX_CHARS);
}

export function sanitizeAcpStderrExcerpt(
  text: string,
  homeDirectory: string,
  environment: NodeJS.ProcessEnv = {},
): string {
  let result = NodeUtil.stripVTControlCharacters(text.replaceAll("\0", ""));
  const homes = [environment.HOME, environment.USERPROFILE, homeDirectory].filter(
    (value): value is string => typeof value === "string" && value.length > 1,
  );
  for (const home of new Set(homes)) {
    result = result.split(home).join("~");
  }
  result = result
    .replace(PAIRING_URL_PATTERN, "[pairing-url]")
    .replace(SIGN_IN_URL_PATTERN, "[sign-in-url]")
    .replace(BEARER_TOKEN_PATTERN, "Bearer [redacted]")
    .replace(BASIC_AUTH_PATTERN, "Authorization: Basic [redacted]")
    .replace(API_KEY_HEADER_PATTERN, "x-api-key: [redacted]")
    .replace(SECRET_TOKEN_PATTERN, "[redacted]");

  const lines = result
    .split(/\r?\n/)
    .map((line) => line.trim().replace(/\s+/g, " "))
    .filter((line) => line.length > 0);
  const lastLine = lines.at(-1);
  if (lastLine === undefined) return "";

  // Wrappers can print the actual failure before their generic exit-status summary.
  if (EXIT_SUMMARY_PATTERN.test(lastLine)) {
    const excerpt = lines.slice(Math.max(0, lines.length - 3)).join(" | ");
    return boundExcerptTail(excerpt);
  }
  const errorIndex = lines.findLastIndex(
    (candidate) => ERROR_LINE_PATTERN.test(candidate) || GLOG_ERROR_LINE_PATTERN.test(candidate),
  );
  if (errorIndex !== -1) {
    return boundExcerptHead(lines[errorIndex]!);
  }
  const joined = lines.join("\n");
  return joined.length <= ACP_STDERR_EXCERPT_MAX_CHARS ? joined : boundExcerptTail(joined);
}
