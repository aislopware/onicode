import { EnvironmentId } from "@onicode/contracts";
import { useRef } from "react";
import { expect, it, vi } from "vite-plus/test";
import { fakeTimers, render } from "../../test/render";
import { useTextFieldSpeech } from "./useTextFieldSpeech";

type Commit = (text: string, selection: { start: number; end: number }) => void;
const speech = vi.hoisted(() => ({ commit: null as Commit | null }));
vi.mock("./useEnvironmentSpeechInput", () => ({
  useEnvironmentSpeechInput: (input: { commitDraft: Commit }) => {
    speech.commit = input.commitDraft;
    return {};
  },
}));
vi.mock("./useDictationShortcut", () => ({ useDictationShortcut: () => null }));
vi.mock("~/components/chat/ComposerSpeechButton", () => ({
  ComposerSpeechButton: () => null,
  ComposerSpeechRecordingPill: () => null,
}));
vi.mock("~/components/chat/VoiceInputSetup", () => ({ VoiceInputSetup: () => null }));
vi.mock("@effect/atom-react", () => ({ useAtomValue: () => [] }));
vi.mock("~/state/server", () => ({ primaryServerKeybindingsAtom: null }));

function Field() {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  useTextFieldSpeech({
    environmentId: EnvironmentId.make("environment"),
    ownerKey: "field",
    text: "",
    textareaRef,
    onTextChange: () => {},
  });
  return (
    <>
      <textarea aria-label="Draft" ref={textareaRef} />
      <input aria-label="Elsewhere" />
      <button type="button">Plain</button>
    </>
  );
}

it("returns focus to the field after dictation only while the user has not moved it", async () => {
  await using screen = await render(<Field />, { router: false }); // the hook reads no route
  const draft = screen.getByRole("textbox", { name: "Draft" });
  const elsewhere = screen.getByRole("textbox", { name: "Elsewhere" });
  await elsewhere.click();
  // Fake frames hold the refocus frame until the test advances it.
  using _timers = fakeTimers();

  speech.commit!("Dictated", { start: 8, end: 8 });
  vi.advanceTimersToNextFrame();
  expect(document.activeElement).toBe(elsewhere.element());

  elsewhere.element().blur();
  speech.commit!("Dictated", { start: 8, end: 8 });
  vi.advanceTimersToNextFrame();
  expect(document.activeElement).toBe(draft.element());

  // A plain button, as when a shortcut started dictation, yields like the composer does.
  screen.getByRole("button", { name: "Plain" }).element().focus();
  speech.commit!("Dictated", { start: 8, end: 8 });
  vi.advanceTimersToNextFrame();
  expect(document.activeElement).toBe(draft.element());
});
