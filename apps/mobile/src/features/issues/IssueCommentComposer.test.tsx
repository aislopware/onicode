import type { IssueCommentAudience } from "@onicode/contracts";
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

const state = vi.hoisted(() => ({
  type: null as ((text: string) => void) | null,
  send: null as (() => void) | null,
  pickAudience: null as ((audience: string) => void) | null,
}));

vi.mock("react-native", () => ({
  Platform: { OS: "ios" },
  View: (props: { children?: ReactNode }) => <>{props.children}</>,
}));
vi.mock("gt-react-native", () => ({ useGT: () => (text: string) => text }));
vi.mock("react-native-reanimated", () => ({
  default: { View: (props: { children?: ReactNode }) => <>{props.children}</> },
}));
vi.mock("../../components/ComposerEditor", async () => {
  const { forwardRef, useImperativeHandle } = await import("react");
  return {
    ComposerEditor: forwardRef((props: { onChangeText: (text: string) => void }, ref) => {
      useImperativeHandle(ref, () => ({ flushPendingInput: async () => {} }));
      state.type = props.onChangeText;
      return null;
    }),
  };
});
vi.mock("../../components/ComposerSurface", () => ({
  COMPOSER_BOTTOM_GUTTER: 0,
  INLINE_EDITOR_MIN_WIDTH: 0,
  ComposerSurface: (props: { children?: ReactNode }) => <>{props.children}</>,
  useCompactComposerEditor: () => ({ editorProps: {} }),
  useComposerChrome: () => ({}),
  useComposerTextStyle: () => ({}),
}));
vi.mock("../../components/ComposerToolbar", () => ({
  ComposerActionButton: (props: { onPress: () => void }) => {
    state.send = props.onPress;
    return null;
  },
  ComposerInlineControl: () => null,
  ComposerToolbarRow: (props: { children?: ReactNode }) => <>{props.children}</>,
}));
vi.mock("../../components/ui/AppText", () => ({ AppText: () => null }));
vi.mock("../../components/ui/Menu", () => ({
  Menu: (props: {
    onPressAction: (event: { nativeEvent: { event: string } }) => void;
    children?: ReactNode;
  }) => {
    state.pickAudience = (event) => props.onPressAction({ nativeEvent: { event } });
    return null;
  },
}));
vi.mock("../settings/appearance/useScaledTextRole", () => ({
  useComposerTextRole: () => ({ lineHeight: 20 }),
}));
vi.mock("../voice-input/ComposerDictationControl", () => {
  const Pass = (props: { children?: ReactNode }) => <>{props.children}</>;
  const Empty = () => null;
  return {
    ComposerDictationCancelAction: Empty,
    ComposerDictationDraftContent: Pass,
    ComposerDictationError: Empty,
    ComposerDictationPrimaryAction: Empty,
    ComposerDictationStartAction: Empty,
    ComposerDictationStatus: Empty,
    ComposerDictationToolbar: Pass,
  };
});
vi.mock("../voice-input/useVoiceInputController", () => ({
  useVoiceInputController: () => ({ state: { phase: "idle" }, blocksSubmission: false }),
}));
vi.mock("../voice-input/voiceInputPresentation", () => ({
  resolveVoiceComposerPresentation: () => ({ statusKind: "idle" }),
}));

import { IssueCommentComposer } from "./IssueCommentComposer";

it.each([
  { typedDuringSend: false, nextAudience: "internal" },
  { typedDuringSend: true, nextAudience: "public" },
])(
  "starts the next comment internal after a customer reply, unless text was typed during the send ($typedDuringSend)",
  async ({ typedDuringSend, nextAudience }) => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const sent: Array<IssueCommentAudience | undefined> = [];
    let posted = Promise.withResolvers<boolean>();
    const onSubmit = (_body: string, audience?: IssueCommentAudience) => {
      sent.push(audience);
      return posted.promise;
    };
    const root = createRoot(document.createElement("div"));
    using _root = { [Symbol.dispose]: () => act(() => root.unmount()) };
    act(() => {
      root.render(
        <IssueCommentComposer
          ownerKey="issue"
          label="Issue"
          blocked={null}
          posting={false}
          audiences
          bottomInset={0}
          onSubmit={onSubmit}
          onHeightChange={() => {}}
          onFocusChange={() => {}}
        />,
      );
    });
    act(() => state.pickAudience!("public"));
    act(() => state.type!("Thanks, fixed."));
    await act(async () => state.send!());
    if (typedDuringSend) act(() => state.type!("Thanks, fixed. One more"));
    await act(async () => {
      posted.resolve(true);
      await posted.promise;
    });

    posted = Promise.withResolvers<boolean>();
    if (!typedDuringSend) act(() => state.type!("Next"));
    await act(async () => state.send!());
    expect(sent).toEqual(["public", nextAudience]);
  },
);
