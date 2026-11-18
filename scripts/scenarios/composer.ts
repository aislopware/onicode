// Composer steps several scenarios repeat: picking a replay recording and the permission mode.
import { expect, type Screen } from "e2e";

/** The dev replay agent's recordings, by the name the model picker shows. */
const RECORDINGS = {
  showcase: "Showcase",
  "todo-list": "Plan and reads",
  approval: "Write approval",
  question: "Question",
  "multi-turn": "Multi turn",
  "command-output": "Command output",
} as const;

export type Permissions = "Supervised" | "Auto-accept edits" | "Auto" | "Full access";

/** Picks the recording the next message replays, and how much it may do without asking. */
export const chooseReplay = async (
  screen: Screen,
  recording: keyof typeof RECORDINGS,
  permissions: Permissions = "Full access",
) => {
  const name = RECORDINGS[recording];
  const picker = screen.getByRole(
    "button",
    new RegExp(`^(${Object.values(RECORDINGS).join("|")})$`),
  );
  await picker.tap();
  // The picker opens on favorites and recent picks; searching reaches every recording.
  const search = screen.getByRole("combobox", "Search models");
  await search.fill(name);
  await screen.getByRole("option", new RegExp(`^${name} `)).tap();
  await expect(picker).toHaveAccessibleName(name);
  // A click selects without closing, so the effort can be set next; Escape closes.
  await search.press("Escape");
  await expect(search).toBeHidden();

  const mode = screen.getByRole("button", /^Permissions: /);
  await mode.tap();
  await screen.getByRole("menuitemradio", permissions).tap();
  await expect(mode).toHaveAccessibleName(`Permissions: ${permissions}`);
};
