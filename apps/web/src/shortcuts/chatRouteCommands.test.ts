import { derivePhysicalProjectKey } from "@onicode/client-runtime/state/project-grouping";
import { DEFAULT_CLIENT_SETTINGS, EnvironmentId, ProjectId } from "@onicode/contracts";
import type { AnyRouter } from "@tanstack/react-router";
import { describe, expect, it, vi } from "vite-plus/test";

import * as commandPalette from "../commandPaletteBus";
import { DraftId, useComposerDraftStore } from "../composerDraftStore";
import * as previewActions from "../components/preview/previewActionBus";
import * as newThread from "../hooks/useHandleNewThread";
import * as settings from "../hooks/useSettings";
import * as storage from "../hooks/useLocalStorage";
import * as threadActions from "../lib/chatThreadActions";
import * as previewFocus from "../lib/previewFocus";
import { isMacPlatform } from "../lib/utils";
import * as entities from "../state/entities";
import * as projectsState from "../state/projects";
import * as session from "../state/session";
import { Atom } from "effect/reactivity";
import type { Project } from "../types";
import { useUiStateStore } from "../uiStateStore";
import { installChatRouteCommands } from "./chatRouteCommands";
import * as dispatcher from "./dispatcher";

function project(id: string): Project {
  return {
    id: ProjectId.make(id),
    environmentId: EnvironmentId.make("local"),
    title: id,
    workspaceRoot: `/projects/${id}`,
    repositoryIdentity: null,
    defaultModelSelection: null,
    createdAt: "2026-01-01T00:00:00.000Z",
    updatedAt: "2026-01-01T00:00:00.000Z",
    scripts: [],
  };
}

describe("new thread commands", () => {
  it.each([
    { mode: "normal", count: 2, command: "chat.new", picker: true },
    { mode: "boring", count: 2, command: "chat.new", picker: true },
    { mode: "boring", count: 1, command: "chat.new", picker: false },
    { mode: "boring", count: 2, command: "chat.newLocal", picker: false },
  ] as const)("$mode sidebar, $count projects, $command: picker=$picker", (scenario) => {
    using _settings = vi.spyOn(settings, "getClientSettings").mockReturnValue({
      ...DEFAULT_CLIENT_SETTINGS,
      boringSidebarEnabled: scenario.mode === "boring",
    });
    using _projects = vi
      .spyOn(entities, "readProjects")
      .mockReturnValue([project("active"), project("idle")].slice(0, scenario.count));
    using _ready = vi
      .spyOn(projectsState, "defaultProjectFallbackReadyAtom")
      .mockReturnValue(Atom.make(true));
    using storedProject = vi.spyOn(storage, "getLocalStorageItem").mockReturnValue("local:active");
    using openPicker = vi.spyOn(commandPalette, "openCommandPalette").mockImplementation(() => {});
    using startLocal = vi.spyOn(threadActions, "startNewThreadFromContext").mockResolvedValue(true);
    using register = vi.spyOn(dispatcher, "registerCommands").mockReturnValue(() => {});
    const router = { state: { matches: [] } } as unknown as AnyRouter;
    using stack = new DisposableStack();
    stack.defer(installChatRouteCommands(router));
    const commands = register.mock.calls[0]![0];
    const event = {
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    } as unknown as KeyboardEvent;

    storedProject.mockReturnValue("local:idle");
    commands[scenario.command]!(event);

    if (scenario.picker) {
      expect(openPicker).toHaveBeenCalledWith({ open: "new-thread-in" });
      expect(startLocal).not.toHaveBeenCalled();
    } else {
      expect(openPicker).not.toHaveBeenCalled();
      expect(startLocal).toHaveBeenCalledOnce();
      expect(startLocal.mock.calls[0]![0].defaultProjectRef).toEqual({
        environmentId: "local",
        projectId: scenario.count === 1 ? "active" : "idle",
      });
    }
  });

  it("chat.new starts in the project the sidebar is filtered to, like its New button", () => {
    using _settings = vi
      .spyOn(settings, "getClientSettings")
      .mockReturnValue(DEFAULT_CLIENT_SETTINGS);
    using _projects = vi
      .spyOn(entities, "readProjects")
      .mockReturnValue([project("active"), project("idle")]);
    using _ready = vi
      .spyOn(projectsState, "defaultProjectFallbackReadyAtom")
      .mockReturnValue(Atom.make(true));
    using _storedProject = vi.spyOn(storage, "getLocalStorageItem").mockReturnValue("local:idle");
    using openPicker = vi.spyOn(commandPalette, "openCommandPalette").mockImplementation(() => {});
    const handleNewThread = vi.fn().mockResolvedValue(null);
    using _handler = vi.spyOn(newThread, "createNewThreadHandler").mockReturnValue(handleNewThread);
    using register = vi.spyOn(dispatcher, "registerCommands").mockReturnValue(() => {});
    using stack = new DisposableStack();
    useUiStateStore.setState({
      sidebarProjectScopeKeys: [derivePhysicalProjectKey(project("active"))],
    });
    stack.defer(() => useUiStateStore.setState({ sidebarProjectScopeKeys: [] }));
    stack.defer(installChatRouteCommands({ state: { matches: [] } } as unknown as AnyRouter));
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn() } as unknown as KeyboardEvent;

    register.mock.calls[0]![0]["chat.new"]!(event);

    expect(openPicker).not.toHaveBeenCalled();
    expect(handleNewThread).toHaveBeenCalledWith({ environmentId: "local", projectId: "active" });
  });
});

describe("preview commands", () => {
  it.each([
    { command: "preview.find", action: "find" },
    { command: "preview.refresh", action: "refresh" },
    { command: "preview.focusUrl", action: "focus-url" },
  ] as const)("$command reaches the right panel on a draft thread", ({ command, action }) => {
    const draftId = DraftId.make("draft-1");
    useComposerDraftStore
      .getState()
      .setProjectDraftThreadId(
        { environmentId: EnvironmentId.make("local"), projectId: ProjectId.make("active") },
        draftId,
      );
    using stack = new DisposableStack();
    stack.defer(() => useComposerDraftStore.getState().clearDraftThread(draftId));
    using _scope = vi.spyOn(session, "readEnvironmentScope").mockReturnValue(true);
    using dispatch = vi.spyOn(previewActions, "dispatchPreviewAction").mockImplementation(() => {});
    using register = vi.spyOn(dispatcher, "registerCommands").mockReturnValue(() => {});
    const router = {
      state: { matches: [{ routeId: "/_chat/draft/$draftId", params: { draftId } }] },
    } as unknown as AnyRouter;
    stack.defer(installChatRouteCommands(router));
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn() } as unknown as KeyboardEvent;

    register.mock.calls[0]![0][command]!(event);

    expect(dispatch).toHaveBeenCalledWith(action);
  });

  it("zooms the preview from a custom chord without browser focus, leaving app zoom alone", () => {
    const draftId = DraftId.make("draft-1");
    useComposerDraftStore
      .getState()
      .setProjectDraftThreadId(
        { environmentId: EnvironmentId.make("local"), projectId: ProjectId.make("active") },
        draftId,
      );
    using stack = new DisposableStack();
    stack.defer(() => useComposerDraftStore.getState().clearDraftThread(draftId));
    using _scope = vi.spyOn(session, "readEnvironmentScope").mockReturnValue(true);
    using _focus = vi.spyOn(previewFocus, "isBrowserSurfaceFocused").mockReturnValue(false);
    using dispatch = vi.spyOn(previewActions, "dispatchPreviewAction").mockImplementation(() => {});
    using register = vi.spyOn(dispatcher, "registerCommands").mockReturnValue(() => {});
    const router = {
      state: { matches: [{ routeId: "/_chat/draft/$draftId", params: { draftId } }] },
    } as unknown as AnyRouter;
    stack.defer(installChatRouteCommands(router));
    const zoomIn = register.mock.calls[0]![0]["preview.zoomIn"]!;
    const mac = isMacPlatform(navigator.platform);
    const press = (init: Partial<KeyboardEvent>) =>
      ({
        metaKey: false,
        ctrlKey: false,
        shiftKey: false,
        altKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
        ...init,
      }) as unknown as KeyboardEvent;

    expect(zoomIn(press({ key: "=", metaKey: mac, ctrlKey: !mac }))).toBe(false);
    expect(dispatch).not.toHaveBeenCalled();

    zoomIn(press({ key: "I", ctrlKey: true, shiftKey: true }));
    expect(dispatch).toHaveBeenCalledWith("zoom-in");
    // Shift+Mod+0 is no app zoom, so a custom binding on it reaches the preview.
    zoomIn(press({ key: ")", code: "Digit0", metaKey: mac, ctrlKey: !mac, shiftKey: true }));
    expect(dispatch).toHaveBeenCalledTimes(2);
  });
});
