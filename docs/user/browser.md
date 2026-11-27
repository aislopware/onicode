# Built-in browser

Onicode has a preview browser beside your conversation, so you and your agents can check local dev servers, annotate UI issues, record flows, and test your sites without leaving the thread. For how agents browse, see [Browser use](./browser-use.md).

## Open and navigate

- Open the right panel in a thread and choose **Browser**, or select a running server in the thread details card.
- **Local and remote**: In the desktop app, tabs for its own environment run in the app. On mobile, and for other environments, tabs stream from the host's headless browser (a one-time download; see [Browser host setup](./remote-access.md#browser-host-setup)). Tabs you open in the desktop app for another environment can also load on your computer ([Browser on a remote environment](./remote-access.md#browser-on-a-remote-environment)).
- **Find on page**: Press `mod+f` in a desktop browser tab, or use its **⋮** menu when the page handles `mod+f` itself ([details](./keybindings.md#find-on-a-browser-page)).
- **Default browser (macOS)**: You can make the desktop app your [default web browser](./default-browser.md).

## Profiles, viewports, and settings

Set the defaults in **Settings → Browser**:

- **Browser profiles**: Keep separate cookie and storage profiles and choose a **Default browser profile**. On desktop you can copy sign-ins from your other browsers ([Import browser sessions](./browser-import.md)).
- **Viewport, zoom, and appearance**: Set **Default browser viewport** (**Fill panel**, **Responsive**, or a fixed size you can rotate), **Default browser zoom**, and **Default browser appearance** (system, light, or dark).
- **Links and search**: Choose **Open links in** and the **Search engine**.

## Pick elements, annotate, and record

- **Element picker and annotations**: Pick elements or annotate regions on the page to attach them to the composer as context chips.
- **Recordings**: Record what happens in a tab from its toolbar. **Browser recording frame rate**, **Show key presses in recordings**, and **Show mouse presses in recordings** are in **Settings → Browser**.

## Agents in the browser

Agents can open tabs, navigate, click, type, read the page, and take screenshots.

- Turn this on or off with **Settings → Browser → Agent browser access**, for an environment or as a project override. A change reaches the next agent session that starts.
- **Floating preview**: Tabs an agent opens float over the chat by default, and closing the right panel floats its browser or device (**Settings → Browser → Auto-show floating preview**). Turn this off to open agent tabs in the right panel and close the panel without a float.
- **Take control**: Click **Take control** before you use an agent's tab, and **Release control** to let the agent continue.
