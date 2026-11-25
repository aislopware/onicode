import type { MenuAction } from "@react-native-menu/menu";

// Menu icons take the app theme's colours; an omitted imageColor falls back to the system tint.
export function withMenuActionIconColors(
  actions: readonly MenuAction[],
  colors: {
    readonly icon: MenuAction["imageColor"];
    readonly destructiveIcon: MenuAction["imageColor"];
  },
): MenuAction[] {
  return actions.map((action) => ({
    ...action,
    ...(action.image
      ? {
          imageColor:
            action.imageColor ??
            (action.attributes?.destructive ? colors.destructiveIcon : colors.icon),
        }
      : {}),
    ...(action.subactions
      ? { subactions: withMenuActionIconColors(action.subactions, colors) }
      : {}),
  }));
}
