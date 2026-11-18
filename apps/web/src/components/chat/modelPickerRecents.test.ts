import { EnvironmentId, ProviderInstanceId } from "@onicode/contracts";
import { describe, expect, it } from "vite-plus/test";

import { readRecentModels, recentModelsToShow, rememberRecentModel } from "./modelPickerRecents";

const model = (slug: string, instance = "codex") => ({
  instanceId: ProviderInstanceId.make(instance),
  slug,
});

it("remembers each environment's picks newest first, once each", () => {
  const environment = EnvironmentId.make("environment-recents");
  for (const slug of ["a", "b", "a"]) rememberRecentModel(environment, model(slug));
  rememberRecentModel(EnvironmentId.make("environment-other"), model("z"));

  expect(readRecentModels(environment)).toEqual([model("a"), model("b")]);
  expect(readRecentModels(null)).toEqual([]);
});

describe("recentModelsToShow", () => {
  const models = ["a", "b", "c", "d", "e", "f"].map((slug) => model(slug));
  const show = (recents: string[], active: string | null, favorites: string[] = []) =>
    recentModelsToShow({
      recents: recents.map((slug) => model(slug)),
      active: active === null ? null : model(active),
      models,
      isFavorite: (candidate) => favorites.includes(candidate.slug),
    }).map((shown) => shown.slug);

  it("lists the newest picks, without favorites or models that are gone", () => {
    expect(show(["b", "gone", "a", "c"], "b", ["a"])).toEqual(["b", "c"]);
  });

  it("always shows the selected model", () => {
    // Picked on another device, so this one never recorded it.
    expect(show(["a", "b"], "f")).toEqual(["f", "a", "b"]);
    // Older than everything the section has room for.
    expect(show(["a", "b", "c", "d", "e"], "e")).toEqual(["a", "b", "c", "e"]);
    // A favorite shows in Favorites instead.
    expect(show(["a", "b"], "f", ["f"])).toEqual(["a", "b"]);
  });

  it("tells one provider's model from another's with the same name", () => {
    expect(
      recentModelsToShow({
        recents: [model("a", "claude")],
        active: null,
        models: [model("a"), model("a", "claude")],
        isFavorite: () => false,
      }),
    ).toEqual([model("a", "claude")]);
  });
});
