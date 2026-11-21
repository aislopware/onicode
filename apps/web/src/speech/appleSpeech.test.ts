import { expect, it, vi } from "vite-plus/test";

it.each([
  { answers: [[]], probes: 1, locales: [] },
  { answers: [["en-US"]], probes: 1, locales: ["en-US"] },
  { answers: [null, ["en-US"]], probes: 2, locales: ["en-US"] },
])("asks for Apple Speech locales until a definite answer: $answers", async (row) => {
  vi.resetModules();
  const supportedLocales = vi.fn<() => Promise<readonly string[] | null>>();
  for (const answer of row.answers) supportedLocales.mockResolvedValueOnce(answer);
  vi.stubGlobal("window", { desktopBridge: { appleSpeech: { supportedLocales } } });
  using _window = { [Symbol.dispose]: () => vi.unstubAllGlobals() };
  const speech = await import("./appleSpeech");
  await speech.probeAppleSpeechLocales();
  await speech.probeAppleSpeechLocales();
  await speech.probeAppleSpeechLocales();
  expect(speech.readAppleSpeechLocales()).toEqual(row.locales);
  expect(supportedLocales).toHaveBeenCalledTimes(row.probes);
});
