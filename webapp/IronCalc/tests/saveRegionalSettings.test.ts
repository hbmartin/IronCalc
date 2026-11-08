import { readFile } from "node:fs/promises";
import { initSync, Model } from "@ironcalc/wasm";
import { beforeAll, expect, test, vi } from "vitest";
import {
  saveAndRedrawRegionalSettings,
  saveRegionalSettings,
} from "../src/components/RightDrawer/RegionalSettings/saveRegionalSettings";

beforeAll(async () => {
  initSync({
    module: await readFile("node_modules/@ironcalc/wasm/wasm_bg.wasm"),
  });
});

function newModel() {
  const model = new Model("settings", "en", "UTC", "en");
  model.setUserInput(0, 1, 1, "=RAND()");
  model.flushSendQueue();
  return model;
}

test("paused context rejection preserves the workbook and can be retried", () => {
  const model = newModel();
  model.pauseEvaluation();
  const bytes = model.toBytes();
  const result = saveRegionalSettings(model, "fr", "Europe/Paris", "en");
  expect(result.error?.code).toMatch(/paused/);
  expect(model.toBytes()).toEqual(bytes);
  expect(model.getLocale()).toBe("en");
  expect(model.getTimezone()).toBe("UTC");
  expect(model.flushSendQueue()).toHaveLength(1);
  model.resumeEvaluation();
  expect(
    saveRegionalSettings(model, "fr", "Europe/Paris", "en").error,
  ).toBeNull();
  expect(model.getLocale()).toBe("fr");
  expect(model.getTimezone()).toBe("Europe/Paris");
});

test.each([
  ["invalid", "Europe/Paris", /locale/],
  ["fr", "Not/A_Timezone", /timezone/],
])(
  "validates every selection before any setter: %s, %s",
  (locale, timezone, error) => {
    const model = newModel();
    const before = model.toBytes();
    const setter = vi.spyOn(model, "setLocale");
    expect(
      saveRegionalSettings(model, locale, timezone, "en").error?.code,
    ).toMatch(error);
    expect(setter).not.toHaveBeenCalled();
    expect(model.toBytes()).toEqual(before);
    expect(model.flushSendQueue()).toHaveLength(1);
  },
);

test("unchanged selections call no setters, including while paused", () => {
  const model = newModel();
  model.pauseEvaluation();
  const locale = vi.spyOn(model, "setLocale");
  const timezone = vi.spyOn(model, "setTimezone");
  const language = vi.spyOn(model, "setLanguage");
  const before = model.toBytes();
  expect(saveRegionalSettings(model, "en", "UTC", "en").error).toBeNull();
  expect(locale).not.toHaveBeenCalled();
  expect(timezone).not.toHaveBeenCalled();
  expect(language).not.toHaveBeenCalled();
  expect(model.toBytes()).toEqual(before);
});

test("language-only changes work while calculation is paused", () => {
  const model = newModel();
  model.pauseEvaluation();
  const before = model.getFormattedCellValue(0, 1, 1);
  expect(saveRegionalSettings(model, "en", "UTC", "es").error).toBeNull();
  expect(model.getLanguage()).toBe("es");
  expect(model.getFormattedCellValue(0, 1, 1)).toBe(before);
  expect(model.flushSendQueue()).toHaveLength(1);
});

test("successful saves invoke each changed setter once", () => {
  const model = newModel();
  const locale = vi.spyOn(model, "setLocale");
  const timezone = vi.spyOn(model, "setTimezone");
  const language = vi.spyOn(model, "setLanguage");
  expect(
    saveRegionalSettings(model, "fr", "Europe/Paris", "es").error,
  ).toBeNull();
  expect(locale).toHaveBeenCalledExactlyOnceWith("fr");
  expect(timezone).toHaveBeenCalledExactlyOnceWith("Europe/Paris");
  expect(language).toHaveBeenCalledExactlyOnceWith("es");
});

test("invalid-settings errors are returned before later setters run", () => {
  const model = newModel();
  vi.spyOn(model, "setLocale").mockImplementation(() => {
    throw new Error(
      "Cannot change calculation context with invalid iterative calculation settings",
    );
  });
  const timezone = vi.spyOn(model, "setTimezone");
  const language = vi.spyOn(model, "setLanguage");
  const before = model.toBytes();
  expect(
    saveRegionalSettings(model, "fr", "Europe/Paris", "es").error?.code,
  ).toMatch(/invalid_iteration/);
  expect(timezone).not.toHaveBeenCalled();
  expect(language).not.toHaveBeenCalled();
  expect(model.toBytes()).toEqual(before);
});

test("timezone-only rejection and non-Error throws become save errors", () => {
  const model = newModel();
  model.pauseEvaluation();
  expect(
    saveRegionalSettings(model, "en", "Europe/Paris", "en").error?.code,
  ).toMatch(/paused/);
  vi.spyOn(model, "setTimezone").mockImplementation(() => {
    throw "settings rejected";
  });
  expect(saveRegionalSettings(model, "en", "Europe/Paris", "en").error).toEqual(
    { code: "save_failed", detail: "settings rejected" },
  );
});

test("invalid language is rejected before any setter", () => {
  const model = newModel();
  const before = model.toBytes();
  const locale = vi.spyOn(model, "setLocale");
  expect(saveRegionalSettings(model, "fr", "Europe/Paris", "invalid")).toEqual({
    error: { code: "invalid_language", value: "invalid" },
    changed: false,
  });
  expect(locale).not.toHaveBeenCalled();
  expect(model.toBytes()).toEqual(before);
});

test.each(["setTimezone", "setLanguage"] as const)(
  "partial success is reported when %s fails",
  (setter) => {
    const model = newModel();
    vi.spyOn(model, setter).mockImplementation(() => {
      throw new Error("later setter failed");
    });
    const result = saveRegionalSettings(model, "fr", "Europe/Paris", "es");
    expect(result.changed).toBe(true);
    expect(result.error?.code).toBe("save_failed");
    expect(model.getLocale()).toBe("fr");
    expect(model.getLanguage()).toBe("en");
  },
);

test("supported formula languages remain accepted by the real engine", () => {
  const model = newModel();
  for (const language of ["en", "es", "fr", "de", "it"]) {
    expect(saveRegionalSettings(model, "en", "UTC", language).error).toBeNull();
    expect(model.getLanguage()).toBe(language);
  }
});

test("redraw follows partial changes and skips rejected unchanged saves", () => {
  const model = newModel();
  const redraw = vi.fn();
  model.pauseEvaluation();
  expect(
    saveAndRedrawRegionalSettings(model, "fr", "Europe/Paris", "en", redraw)
      .changed,
  ).toBe(false);
  expect(redraw).not.toHaveBeenCalled();
  model.resumeEvaluation();
  const timezone = vi.spyOn(model, "setTimezone").mockImplementation(() => {
    throw new Error("later failure");
  });
  expect(
    saveAndRedrawRegionalSettings(model, "fr", "Europe/Paris", "en", redraw)
      .error?.code,
  ).toBe("save_failed");
  expect(redraw).toHaveBeenCalledTimes(1);
  timezone.mockRestore();
  expect(
    saveAndRedrawRegionalSettings(model, "fr", "Europe/Paris", "en", redraw)
      .error,
  ).toBeNull();
  expect(redraw).toHaveBeenCalledTimes(2);
});

test("settings errors have translated presentation in every bundled language", async () => {
  const { default: i18n } = await import("../src/i18n");
  await i18n.init();
  for (const language of ["en-US", "es-ES", "fr-FR", "de-DE", "it-IT"]) {
    const bundle = i18n.getResourceBundle(language, "translation");
    for (const code of [
      "invalid_locale",
      "invalid_timezone",
      "invalid_language",
      "paused",
      "invalid_iteration",
      "save_failed",
    ]) {
      expect(bundle.regional_settings.errors[code]).toBeTypeOf("string");
      expect(bundle.regional_settings.errors[code].length).toBeGreaterThan(10);
    }
  }
});

test.each([
  ["Cannot change locale while evaluation is paused", "paused"],
  ["Cannot change timezone while evaluation is paused", "paused"],
  ["Cannot change calculation context while evaluation is paused", "paused"],
  [
    "Cannot change calculation context with invalid iterative calculation settings",
    "invalid_iteration",
  ],
  ["unrelated paused request", "save_failed"],
  ["unrelated invalid iterative calculation settings request", "save_failed"],
  [
    "Cannot change calculation context while evaluation is paused: unexpected detail",
    "save_failed",
  ],
])("classifies only canonical engine rejections: %s", (message, code) => {
  for (const asError of [false, true]) {
    const model = newModel();
    vi.spyOn(model, "setLocale").mockImplementation(() => {
      throw asError ? new Error(message) : message;
    });
    const redraw = vi.fn();
    const result = saveAndRedrawRegionalSettings(
      model,
      "fr",
      "UTC",
      "en",
      redraw,
    );
    expect(result.error).toEqual({ code, detail: message });
    expect(result.changed).toBe(false);
    expect(redraw).not.toHaveBeenCalled();
  }
});

test("build-time formula language IDs match the engine source and real WASM", async () => {
  const source = JSON.parse(
    await readFile(
      new URL("../../../generate_language/src/languages.json", import.meta.url),
      "utf8",
    ),
  );
  expect([...__IC_FORMULA_LANGUAGE_IDS__].sort()).toEqual(
    Object.keys(source).sort(),
  );
  const model = newModel();
  for (const language of __IC_FORMULA_LANGUAGE_IDS__) {
    expect(saveRegionalSettings(model, "en", "UTC", language).error).toBeNull();
    expect(model.getLanguage()).toBe(language);
  }
});
