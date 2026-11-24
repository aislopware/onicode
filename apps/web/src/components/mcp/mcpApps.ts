import chatgptLogo from "./logos/chatgpt.png";
import claudeLogo from "./logos/claude.png";
import grokBotLogo from "./logos/grokbot.png";
import hermesLogo from "./logos/hermes.png";
import instinctLogo from "./logos/instinct.png";
import museLogo from "./logos/muse.png";
import openClawLogo from "./logos/openclaw.png";
import pokeLogo from "./logos/poke.png";
import { msg } from "gt-react";

import { m, type MessageResolver } from "~/i18n/messages";
import { ClaudeAI, type Icon, OpenAI } from "../Icons";

/** How the Connect step hands Onicode's MCP URL to an app. Every `link` is https. */
export type McpAppHandoff =
  /** Opens the app's add form with Onicode filled in. */
  | { readonly kind: "open"; readonly link: (mcpUrl: string) => string }
  /** Copies the URL, then opens the page it goes on. */
  | { readonly kind: "copy-url"; readonly link: string }
  /** Copies a message for the app's chat, then opens the app. */
  | {
      readonly kind: "copy-message";
      readonly link: string;
      readonly message: (mcpUrl: string) => string;
    }
  /** Commands to run where the app runs. */
  | { readonly kind: "commands"; readonly commands: (mcpUrl: string) => readonly string[] };

/**
 * Where the app calls Onicode from. A cloud app reaches only a public address, so it needs
 * public agents on; an app on a device of yours can use the address this device uses.
 */
export type McpAppReach = "cloud" | "device";

export interface McpApp {
  readonly id: string;
  readonly name: string;
  readonly logo: string | Icon;
  readonly reach: McpAppReach;
  readonly handoff: McpAppHandoff;
  /** "Set up another way", next to the URL. `msg()` strings. */
  readonly steps: readonly string[];
  /** One line under the primary action. A `msg()` string. */
  readonly note?: string;
  readonly docs?: string;
}

const encoded = encodeURIComponent;

export const MCP_APPS: readonly McpApp[] = [
  {
    id: "claude-code",
    name: "Claude Code",
    logo: ClaudeAI,
    reach: "device",
    handoff: {
      kind: "commands",
      commands: (url) => [
        `claude mcp add --transport http onicode ${url}`,
        "claude mcp login onicode",
      ],
    },
    steps: [
      msg("Add a remote MCP server named onicode with the URL."),
      msg("Run claude mcp login onicode, or /mcp inside Claude Code."),
    ],
    docs: "https://code.claude.com/docs/en/mcp",
  },
  {
    id: "codex",
    name: "Codex",
    logo: OpenAI,
    reach: "device",
    handoff: {
      kind: "commands",
      commands: (url) => [`codex mcp add onicode --url ${url}`, "codex mcp login onicode"],
    },
    steps: [
      msg("Add onicode under mcp_servers in ~/.codex/config.toml, with the URL."),
      msg("Run codex mcp login onicode."),
    ],
    docs: "https://developers.openai.com/codex/mcp",
  },
  {
    id: "claude",
    name: "Claude",
    logo: claudeLogo,
    reach: "cloud",
    handoff: {
      kind: "open",
      link: (url) =>
        `https://claude.ai/customize/connectors?modal=add-custom-connector&connectorName=Onicode&connectorUrl=${encoded(url)}`,
    },
    steps: [
      msg("In Claude, open Customize, then Connectors."),
      msg("Choose Add custom connector and paste the URL."),
      msg("The Free plan allows one custom connector."),
    ],
    docs: "https://claude.com/docs/connectors/building/directory-vs-custom",
  },
  {
    id: "chatgpt",
    name: "ChatGPT",
    logo: chatgptLogo,
    reach: "cloud",
    handoff: { kind: "copy-url", link: "https://chatgpt.com/plugins" },
    steps: [
      msg("Open chatgpt.com/plugins on the web, then + and Add custom MCP server."),
      msg("Paste the URL. Choose OAuth."),
      msg("Create it as a plugin, then install it."),
    ],
    docs: "https://developers.openai.com/api/docs/guides/custom-mcp-server",
  },
  {
    id: "poke",
    name: "Poke",
    logo: pokeLogo,
    reach: "cloud",
    handoff: {
      kind: "open",
      link: (url) => `https://poke.com/integrations/new?name=Onicode&url=${encoded(url)}`,
    },
    steps: [msg("Open poke.com/integrations/new."), msg("Paste the URL and add the integration.")],
    docs: "https://poke.com/docs/mcp-servers",
  },
  {
    id: "grok-bot",
    name: "Grok Bot",
    logo: grokBotLogo,
    reach: "cloud",
    handoff: {
      kind: "copy-message",
      link: "https://x.ai/bot",
      message: (url) => `Add the MCP server ${url} and connect it.`,
    },
    steps: [msg("Send this message to any of your Grok Bots.")],
    note: msg("Shared with all your Grok Bots."),
    docs: "https://docs.x.ai/grok-bot/computer-and-apps",
  },
  {
    id: "muse",
    name: "Muse",
    logo: museLogo,
    reach: "cloud",
    handoff: {
      kind: "copy-message",
      link: "https://muse.ai",
      message: (url) => `Create a custom connector for the MCP server ${url}.`,
    },
    steps: [msg("Send this message to Muse.")],
    docs: "https://www.meta.com/help/artificial-intelligence/1687253048996149/",
  },
  {
    id: "instinct",
    name: "Instinct",
    logo: instinctLogo,
    reach: "cloud",
    handoff: {
      kind: "copy-message",
      link: "https://app.instinct.com",
      message: (url) => `Connect the MCP server ${url}.`,
    },
    steps: [msg("In Instinct, open Settings, then MCP connections, if your account has it.")],
  },
  {
    id: "openclaw",
    name: "OpenClaw",
    logo: openClawLogo,
    reach: "device",
    handoff: {
      kind: "commands",
      commands: (url) => [
        // Its MCP commands refuse to write while the Gateway runs.
        "openclaw gateway stop",
        `openclaw mcp add onicode --url ${url} --transport streamable-http --auth oauth`,
        "openclaw mcp login onicode",
        "openclaw gateway start",
      ],
    },
    steps: [
      msg("Open the Control UI, then + and Plugins, and choose Add MCP server."),
      msg("Paste the URL, and choose Streamable HTTP and OAuth."),
      msg("Select Sign in for it under Settings → MCP."),
    ],
    docs: "https://docs.openclaw.ai/tools/mcp",
  },
  {
    id: "hermes-agent",
    name: "Hermes Agent",
    logo: hermesLogo,
    reach: "device",
    handoff: {
      kind: "commands",
      commands: (url) => [
        // `add` signs in on its first connection; the default 30 s is too short to approve.
        `hermes mcp add onicode --url ${url} --auth oauth --connect-timeout 315`,
      ],
    },
    steps: [
      msg("Add onicode under mcp_servers in ~/.hermes/config.yaml, with the URL and auth: oauth."),
      msg("Run hermes mcp login onicode."),
    ],
    note: msg("On a remote host, paste the final URL back when asked."),
    docs: "https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp",
  },
];

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

/** MCP clients send OAuth credentials only over https or to this computer. */
export function signInWorksAt(mcpUrl: string): boolean {
  const url = URL.parse(mcpUrl);
  return url !== null && (url.protocol === "https:" || LOOPBACK_HOSTS.has(url.hostname));
}

/** The "Something else…" choice: the URL and general steps only. */
export const OTHER_MCP_APP_STEPS = [msg("Add a remote MCP server with OAuth.")] as const;

/**
 * The primary button for `app`: its label, the https page it opens, and what it copies first.
 * Null for apps set up with commands.
 */
export function mcpHandoffAction(app: McpApp, mcpUrl: string, resolve: MessageResolver = m) {
  const { handoff } = app;
  switch (handoff.kind) {
    case "open":
      return {
        label: resolve(msg("Open in {app}", { app: app.name })),
        link: handoff.link(mcpUrl),
        copy: null,
      };
    case "copy-url":
      return {
        label: resolve(msg("Copy URL and open {app}", { app: app.name })),
        link: handoff.link,
        copy: { text: mcpUrl, title: resolve(msg("URL copied")) },
      };
    case "copy-message": {
      const message = handoff.message(mcpUrl);
      return {
        label: resolve(msg("Copy message and open {app}", { app: app.name })),
        link: handoff.link,
        copy: { text: message, title: resolve(msg("Message copied")) },
      };
    }
    case "commands":
      return null;
  }
}
