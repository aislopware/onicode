# Transcript web links

Click an HTTP(S) link in an agent message to open and select a **new Browser tab** in that conversation. The right panel opens if necessary. Links in a subagent transcript belong to its containing conversation. A pending activation is discarded if the selected conversation has changed.

Right-click a link for **Open in Zeron**, **Open in external browser**, or **Copy link address**. The same menu includes **Open links in Zeron**, which controls normal click and Enter/Space activation and is enabled by default. The three explicit actions remain available regardless of that preference. Tab and Shift+Tab move through links and other controls when an input or completion menu does not consume the key. Enter or Space activates a focused link. Shift+F10 opens its action menu; arrows or Tab move through the actions, and Escape dismisses the menu.

A github.com pull request link follows **Open pull requests in** (Settings → General → Links) while Open links in Zeron is on: **Pull request view** opens Zeron's own view as a tab in the conversation's pane, **In-app browser** opens a new Browser tab, and **Default browser** opens the system browser. With Open links in Zeron off, it opens in the system browser like any other link. **Open in Zeron** opens the pull request view when that is the chosen destination, and a Browser tab otherwise.

Hover or keyboard focus shows the full destination, including when the Markdown label says something different. The compact tooltip fits the destination text up to 360 px, wraps long destinations, and scrolls within the window. Copy link address is available in the context menu. Opening that menu hides the tooltip and cancels pending hover disclosure until the menu closes. Inspecting a link makes no network requests. Scrolling the transcript, changing conversations, or removing a link dismisses its disclosure. So does a press anywhere else, which still reaches what was pressed, another link in the same message included.

Long web links are shortened with an ellipsis according to the actual available text width, including inside table cells. Navigation, destination disclosure, and Copy link address retain the complete URL. Text selection uses the original label: selecting an entire shortened URL copies the full URL, and selecting its visible prefix copies that prefix. Selecting across the ellipsis includes the omitted text. Labels, styles, and selection positions update when the window width or streaming content changes.

Only explicit HTTP(S) web addresses without embedded credentials, control characters, whitespace, backslashes, or malformed percent escapes can navigate. Rejected links never fall through to the OS opener. Workspace/file links keep their file-opening behavior. Markdown file previews also open web links in a new Browser tab belonging to the file's conversation, with the same explicit external/copy actions. File previews retain their local-file, anchor, and mail-link handling; their labels are not visually truncated.

The Browser tab is WebKit, hosted as a native view in the window's composition (see [rendering](onicode/decisions/rendering.md)). On iOS, valid web links open in the system browser.

## Reproducible validation

The fixture uses synthetic chat data, temporary app storage, and a local HTTP server. Its Markdown corpus includes short and long URLs, misleading labels, rejected destinations, Unicode, and tables:

- [Fixture Markdown](../crates/ui/examples/browser-fixture/transcript-links.md)
- [Native scenario](../crates/ui/examples/browser-fixture/transcript_links.rs)

```sh
cargo build -p onicode-ui --example browser-fixture --features browser-fixture
ONICODE_TRANSCRIPT_LINK_FIXTURE_ONLY=1 \
  scripts/run-macos-browser-fixture.sh target/debug/examples/browser-fixture \
  /tmp/onicode-transcript-links
```

Unit and rendered tests cover external routing without launching the system browser, stale sessions, rejected destinations, keyboard navigation, selection drags, Unicode, resizing, tables, and streaming offset maps.
