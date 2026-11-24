import type { SignInView } from "./McpOAuth.ts";

const escapeHtml = (value: string) =>
  value.replace(
    /[&<>"']/gu,
    (character) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]!,
  );

/** Static pages: no script, no outside requests, never framed. */
export const PAGE_HEADERS = {
  "content-security-policy":
    "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'",
  "x-frame-options": "DENY",
  "cache-control": "no-store",
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
};

const WAITING_REFRESH_SECONDS = 2;

const STYLES = `
  :root { color-scheme: light dark; font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 32px 16px; background: #f6f7f9; color: #17191f; }
  main { width: min(100%, 440px); }
  h1 { margin: 0 0 8px; font-size: 20px; }
  p { margin: 0 0 12px; line-height: 1.5; color: #4b5060; }
  .code { margin: 16px 0; font: 600 28px/1.2 ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing: 0.08em; color: inherit; }
  .note { font-size: 13px; }
  .warn { color: #a15c00; }
  a { color: inherit; }
  form { display: grid; gap: 8px; margin: 16px 0; }
  input { font: 16px ui-monospace, SFMono-Regular, Menlo, monospace; padding: 8px 10px; border: 1px solid #c9ccd4; border-radius: 6px; background: transparent; color: inherit; text-transform: uppercase; }
  button { font: inherit; padding: 8px 12px; border: 0; border-radius: 6px; background: #17191f; color: #fff; cursor: pointer; }
  .error { color: #b42318; }
  @media (prefers-color-scheme: dark) {
    body { background: #0f1115; color: #e6e8ee; }
    p { color: #a0a6b4; }
    .warn { color: #f0a840; }
    input { border-color: #3a3f4b; }
    button { background: #e6e8ee; color: #0f1115; }
    .error { color: #f97066; }
  }
`;

const shell = (title: string, body: string, head = "") => `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta name="color-scheme" content="light dark" />
    ${head}
    <title>${escapeHtml(title)}</title>
    <style>${STYLES}</style>
  </head>
  <body>
    <main>${body}</main>
  </body>
</html>`;

export const consentPath = (id: string) => `/oauth/mcp/consent?id=${encodeURIComponent(id)}`;
const consentCodePath = (id: string) => `/oauth/mcp/consent/code?id=${encodeURIComponent(id)}`;

const destination = (signIn: SignInView) =>
  signIn.loopbackRedirect
    ? `<p class="note">It returns to an app on this computer.</p>`
    : `<p class="note warn">It returns to ${escapeHtml(signIn.redirectHost)}, which gets this access. Approve only if you started this sign-in there.</p>`;

export function renderWaitingPage(signIn: SignInView): string {
  return shell(
    `Allow ${signIn.clientName}?`,
    `<h1>Allow ${escapeHtml(signIn.clientName)} to use Onicode?</h1>
    <p>Approve this sign-in in the Onicode app on any of your devices. Check that it shows this code:</p>
    <p class="code">${escapeHtml(signIn.matchCode)}</p>
    ${destination(signIn)}
    <p>Waiting for approval. This page updates by itself.</p>
    <p class="note"><a href="${escapeHtml(consentCodePath(signIn.id))}">No app at hand? Enter an agent code instead.</a></p>`,
    `<meta http-equiv="refresh" content="${WAITING_REFRESH_SECONDS};url=${escapeHtml(consentPath(signIn.id))}" />`,
  );
}

export function renderCodePage(signIn: SignInView, error?: string): string {
  return shell(
    `Allow ${signIn.clientName}?`,
    `<h1>Allow ${escapeHtml(signIn.clientName)} to use Onicode?</h1>
    <p>Enter an agent code. Create one in Onicode under Settings → Connections, or run <code>onicode auth agent code</code> on the machine running Onicode. The code sets the access this app gets.</p>
    ${destination(signIn)}
    <form method="post" action="/oauth/mcp/consent/code">
      <input type="hidden" name="id" value="${escapeHtml(signIn.id)}" />
      <input name="code" autocomplete="off" autocapitalize="characters" spellcheck="false" required aria-label="Agent code" placeholder="SUPERVISED-…" />
      <button type="submit">Approve</button>
    </form>
    ${error === undefined ? "" : `<p class="error" role="alert">${escapeHtml(error)}</p>`}
    <p class="note"><a href="${escapeHtml(consentPath(signIn.id))}">Approve in the app instead.</a></p>`,
  );
}

export function renderErrorPage(description: string): string {
  return shell(
    "Sign-in failed",
    `<h1>This sign-in cannot continue</h1>
    <p>${escapeHtml(description)}</p>
    <p>Close this page and start the sign-in again from your agent.</p>`,
  );
}
