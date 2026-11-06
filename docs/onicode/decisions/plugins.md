# No plugin system, no commit credit

Onicode has no plugins. Zeron built them for its zgx app: Luau packages with a VM per plugin service, signing keys
registered on zeron's edge, consent reviews, templates that replace the interface, themes as data, plugin tools and
native MCP servers handed to agents. Onicode dropped zgx and its window never loaded any of it, so the engine,
RPC, MCP and CLI surfaces for it are gone, with `crates/plugin-guard` and the run request's `extra_mcp` list. An
agent gets Onicode's own MCP server and whatever the person configured in the agent itself.

Agents' commits are not credited to Onicode: the `Co-authored-by: zeronsh[bot]` git hook and its setting are gone.
