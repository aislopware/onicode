/**
 * The cards to end so each environment keeps one. An environment updates a single card, and a
 * second start, sent because the first was never answered, leaves one nothing will ever update.
 */
export function duplicateActivityIds(
  activities: ReadonlyArray<{
    readonly id: string;
    readonly environmentId: string;
    readonly updatedAt: string;
  }>,
): ReadonlySet<string> {
  const kept = new Map<string, { readonly id: string; readonly updatedAt: string }>();
  const duplicates = new Set<string>();
  for (const activity of activities) {
    const current = kept.get(activity.environmentId);
    if (current !== undefined && activity.updatedAt <= current.updatedAt) {
      duplicates.add(activity.id);
      continue;
    }
    if (current !== undefined) duplicates.add(current.id);
    kept.set(activity.environmentId, activity);
  }
  return duplicates;
}
