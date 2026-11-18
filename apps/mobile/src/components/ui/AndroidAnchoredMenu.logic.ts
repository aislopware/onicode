import type { MenuAction } from "@react-native-menu/menu";

const menuKey = (action: MenuAction) => action.id ?? action.title;

/** The opened submenus as they are in `actions` now; one that lost its choices closes. */
export function currentMenuPath(
  actions: readonly MenuAction[],
  opened: readonly MenuAction[],
): readonly MenuAction[] {
  const path: MenuAction[] = [];
  let level = actions;
  for (const parent of opened) {
    const current = level.find((action) => menuKey(action) === menuKey(parent));
    if (!current?.subactions?.length) break;
    path.push(current);
    level = current.subactions;
  }
  return path;
}
