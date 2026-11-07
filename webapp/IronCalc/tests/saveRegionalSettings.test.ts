import { readFile } from "node:fs/promises";
import { initSync, Model } from "@ironcalc/wasm";
import { beforeAll, expect, test, vi } from "vitest";
import { saveRegionalSettings } from "../src/components/RightDrawer/RegionalSettings/saveRegionalSettings";

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
  expect(result.error).toMatch(/paused/);
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
    expect(saveRegionalSettings(model, locale, timezone, "en").error).toMatch(
      error,
    );
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
  expect(saveRegionalSettings(model, "fr", "Europe/Paris", "es").error).toMatch(
    /invalid iterative calculation settings/,
  );
  expect(timezone).not.toHaveBeenCalled();
  expect(language).not.toHaveBeenCalled();
  expect(model.toBytes()).toEqual(before);
});

test("timezone-only rejection and non-Error throws become save errors", () => {
  const model = newModel();
  model.pauseEvaluation();
  expect(saveRegionalSettings(model, "en", "Europe/Paris", "en").error).toMatch(
    /paused/,
  );
  vi.spyOn(model, "setTimezone").mockImplementation(() => {
    throw "settings rejected";
  });
  expect(saveRegionalSettings(model, "en", "Europe/Paris", "en").error).toBe(
    "settings rejected",
  );
});
