import { describe, expect, it } from "vite-plus/test";
import { stdioMcpServers, withAgentDeviceEnvironment } from "./mcpSession.ts";

describe("device CLI environment", () => {
  it("preserves provider credentials and commands while routing devices to the owned daemon", () => {
    const environment = withAgentDeviceEnvironment(
      { PATH: "/provider/bin:/usr/bin", PROVIDER_KEY: "fixture" },
      {
        agentDeviceEnvironment: {
          PATH: "/onicode/device/bin",
          PATH_SEPARATOR: ":",
          AGENT_DEVICE_DAEMON_BASE_URL: "http://127.0.0.1:9000",
          AGENT_DEVICE_DAEMON_AUTH_TOKEN: "fixture-device",
        },
      },
    );
    expect(environment).toEqual({
      PATH: "/onicode/device/bin:/provider/bin:/usr/bin",
      PROVIDER_KEY: "fixture",
      AGENT_DEVICE_DAEMON_BASE_URL: "http://127.0.0.1:9000",
      AGENT_DEVICE_DAEMON_AUTH_TOKEN: "fixture-device",
    });
  });

  it("does not grant CLI access when device access was not supplied", () => {
    const environment = { PATH: "/usr/bin", PROVIDER_KEY: "fixture" };
    expect(withAgentDeviceEnvironment(environment, undefined)).toBe(environment);
    expect(withAgentDeviceEnvironment(environment, {})).toBe(environment);
  });
});

describe("stdioMcpServers", () => {
  it("names each present server as its tools are named, computer first", () => {
    const computer = { command: "cua-driver", args: ["mcp"], env: {} };
    const browser = { command: "obu", args: ["mcp"], env: {} };
    expect(stdioMcpServers({ browserMcpServer: browser, computerMcpServer: computer })).toEqual([
      ["computer", computer],
      ["browser", browser],
    ]);
    expect(stdioMcpServers({ browserMcpServer: browser })).toEqual([["browser", browser]]);
    expect(stdioMcpServers({})).toEqual([]);
    expect(stdioMcpServers(undefined)).toEqual([]);
  });
});
