import {
  AuthAccessWriteScope,
  type AuthMcpClientAccess,
  AuthOrchestrationReadScope,
  type ManagedPolicySummary,
  mcpClientScopes,
} from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  agentAccessDenials,
  agentSessionAccess,
  closestGrantable,
  deviceMcpUrl,
  publicMcpUrl,
} from "./agentAccess.logic";

const session = (levels: ReadonlyArray<AuthMcpClientAccess>) => ({
  authenticated: true,
  permissions: [AuthAccessWriteScope, ...levels.flatMap((level) => mcpClientScopes(level))],
});

describe("agentAccessDenials", () => {
  it("denies only the levels the session cannot grant", () => {
    // Supervised and the auto levels need the same scopes; full access also writes settings.
    const denied = agentAccessDenials({
      session: session(["approval-required"]),
      canGrant: true,
      policy: undefined,
      environmentLabel: "Studio",
    });
    expect([...denied]).toEqual([["full-access", "This device cannot grant this on Studio."]]);
  });

  it("denies every level without the Manage access permission", () => {
    const denied = agentAccessDenials({
      session: { authenticated: true, scopes: [AuthOrchestrationReadScope] },
      canGrant: false,
      policy: undefined,
      environmentLabel: "Studio",
    });
    expect(denied.size).toBe(5);
    expect(denied.get("read-only")).toBe(
      "This device needs the Manage access permission on Studio.",
    );
  });

  it("applies the managed runtime-mode ceiling above read-only", () => {
    const denied = agentAccessDenials({
      session: session(["full-access"]),
      canGrant: true,
      policy: {
        organization: "Acme",
        lockedSettings: [],
        limits: { maxRuntimeMode: "approval-required" },
      } satisfies ManagedPolicySummary,
      environmentLabel: "Studio",
    });
    expect([...denied.keys()]).toEqual(["auto-accept-edits", "auto", "full-access"]);
  });
});

describe("closestGrantable", () => {
  it.each([
    { wanted: "approval-required", denied: [], expected: "approval-required" },
    { wanted: "approval-required", denied: ["approval-required"], expected: "read-only" },
    { wanted: "auto", denied: ["auto", "auto-accept-edits"], expected: "approval-required" },
    {
      wanted: "approval-required",
      denied: ["read-only", "approval-required"],
      expected: null,
    },
  ] as const)("$wanted without $denied gives $expected", ({ wanted, denied, expected }) => {
    expect(closestGrantable(wanted, new Map(denied.map((level) => [level, "no"])))).toBe(expected);
  });
});

describe("deviceMcpUrl", () => {
  it.each([
    { addresses: ["https://studio.tail1.ts.net/"], expected: "https://studio.tail1.ts.net/mcp" },
    // Plain http off loopback cannot finish an OAuth sign-in, so the https route wins.
    {
      addresses: ["http://100.64.1.2:3773", "https://studio.tail1.ts.net"],
      expected: "https://studio.tail1.ts.net/mcp",
    },
    {
      addresses: ["http://127.0.0.1:3773", "https://studio.tail1.ts.net"],
      expected: "https://studio.tail1.ts.net/mcp",
    },
    { addresses: ["http://localhost:3773"], expected: "http://localhost:3773/mcp" },
    { addresses: ["http://192.168.1.4:3773", "http://100.64.1.2:3773"], expected: null },
    { addresses: [null, "studio-ssh"], expected: null },
  ])("$addresses gives $expected", ({ addresses, expected }) => {
    expect(deviceMcpUrl(addresses)).toBe(expected);
  });
});

describe("publicMcpUrl", () => {
  it.each([
    {
      agents: true,
      status: { _tag: "on", origin: "https://a.ts.net" },
      expected: "https://a.ts.net/mcp",
    },
    { agents: false, status: { _tag: "on", origin: "https://a.ts.net" }, expected: null },
    { agents: true, status: { _tag: "starting" }, expected: null },
    { agents: true, status: null, expected: null },
  ] as const)("agents $agents, $status gives $expected", ({ agents, status, expected }) => {
    expect(publicMcpUrl(agents, status)).toBe(expected);
  });
});

describe("agentSessionAccess", () => {
  it.each([
    { level: "read-only", expected: "read-only" },
    { level: "full-access", expected: "full-access" },
    // Supervised, auto-accept edits and auto share their scopes.
    { level: "approval-required", expected: null },
    { level: "auto", expected: null },
  ] as const)("$level reads as $expected", ({ level, expected }) => {
    expect(agentSessionAccess(mcpClientScopes(level))).toBe(expected);
  });

  it("does not name a level for extra scopes", () => {
    expect(agentSessionAccess([...mcpClientScopes("read-only"), AuthAccessWriteScope])).toBe(null);
  });
});
