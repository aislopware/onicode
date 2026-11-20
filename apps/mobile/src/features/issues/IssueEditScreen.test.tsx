import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  focused: true,
  goBackCalls: 0,
  edit: null as ((value: string) => void) | null,
  save: null as (() => void) | null,
  update: Promise.resolve<object | null>(null),
}));

vi.mock("@react-navigation/native", () => ({
  useNavigation: () => ({
    goBack: () => state.goBackCalls++,
    isFocused: () => state.focused,
  }),
}));
vi.mock("gt-react-native", () => ({ useGT: () => (text: string) => text }));
vi.mock("../../components/ui/AppText", () => ({
  AppText: () => null,
  AppTextInput: (props: { onChangeText: (value: string) => void }) => {
    state.edit = props.onChangeText;
    return null;
  },
}));
vi.mock("../../components/ui/Button", () => ({
  Button: (props: { onPress: () => void }) => {
    state.save = props.onPress;
    return null;
  },
}));
vi.mock("../../state/issues", () => ({
  issueEnvironment: { detail: () => null, update: null },
  refreshIssueLists: () => {},
}));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: () => ({ data: { title: "Old title", body: "" }, error: null }),
}));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: () => () => state.update }));
vi.mock("../pull-requests/PullRequestSheetContent", () => ({
  PullRequestSheetContent: (props: { children: ReactNode; footer: ReactNode }) => (
    <>
      {props.footer}
      {props.children}
    </>
  ),
}));
vi.mock("./issueParams", () => ({ issueTarget: () => ({ environmentId: "e", input: {} }) }));
vi.mock("./issuePresentation", () => ({
  issueCommandError: () => null,
  useIssueAccess: () => ({ edit: true }),
}));

import { IssueEditScreen } from "./IssueEditScreen";

it.each([
  { focused: true, goBackCalls: 1 },
  { focused: false, goBackCalls: 0 },
])(
  "navigates back after a save only while the sheet keeps focus ($focused)",
  async ({ focused, goBackCalls }) => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    state.focused = true;
    state.goBackCalls = 0;
    const update = Promise.withResolvers<object | null>();
    state.update = update.promise;
    const root = createRoot(document.createElement("div"));
    using _root = { [Symbol.dispose]: () => act(() => root.unmount()) };
    act(() => {
      root.render(<IssueEditScreen route={{ params: { field: "title" } } as never} />);
    });
    act(() => state.edit!("New title"));
    act(() => state.save!());
    state.focused = focused;

    await act(async () => {
      update.resolve({});
      await update.promise;
    });
    expect(state.goBackCalls).toBe(goBackCalls);
  },
);
