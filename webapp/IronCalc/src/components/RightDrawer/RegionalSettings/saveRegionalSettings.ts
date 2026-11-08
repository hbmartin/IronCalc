import type { Model } from "@ironcalc/wasm";
import { getAllTimezones, getSupportedLocales } from "@ironcalc/wasm";
import { formulaLanguageIds } from "../../../generated/formulaLanguageIds.ts";

export type RegionalSettingsError = {
  code:
    | "invalid_locale"
    | "invalid_timezone"
    | "invalid_language"
    | "paused"
    | "invalid_iteration"
    | "save_failed";
  value?: string;
  detail?: string;
};
export type RegionalSettingsSaveResult = {
  error: RegionalSettingsError | null;
  changed: boolean;
};
const formulaLanguages = new Set(formulaLanguageIds);

// Rust/binding APIs still return strings. Match the complete canonical errors;
// unrelated errors containing these words must use the unexpected-failure path.
const rejectionCodes = new Map<string, RegionalSettingsError["code"]>([
  ["Cannot change locale while evaluation is paused", "paused"],
  ["Cannot change timezone while evaluation is paused", "paused"],
  ["Cannot change calculation context while evaluation is paused", "paused"],
  [
    "Cannot change calculation context with invalid iterative calculation settings",
    "invalid_iteration",
  ],
]);

type RegionalModel = Pick<
  Model,
  | "getLocale"
  | "getTimezone"
  | "getLanguage"
  | "setLocale"
  | "setTimezone"
  | "setLanguage"
>;

/** Validate selectable values before the first setter can change the workbook. */
export function saveRegionalSettings(
  model: RegionalModel,
  locale: string,
  timezone: string,
  language: string,
): RegionalSettingsSaveResult {
  const before = [model.getLocale(), model.getTimezone(), model.getLanguage()];
  let error: RegionalSettingsError | null = null;
  try {
    const localeChanged = locale !== before[0];
    const timezoneChanged = timezone !== before[1];
    if (localeChanged && !getSupportedLocales().includes(locale)) {
      error = { code: "invalid_locale", value: locale };
    } else if (timezoneChanged && !getAllTimezones().includes(timezone)) {
      error = { code: "invalid_timezone", value: timezone };
    } else if (language !== before[2] && !formulaLanguages.has(language)) {
      error = { code: "invalid_language", value: language };
    } else {
      if (localeChanged) {
        model.setLocale(locale);
      }
      if (timezoneChanged) {
        model.setTimezone(timezone);
      }
      if (language !== before[2]) {
        model.setLanguage(language);
      }
    }
  } catch (cause) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    const code = rejectionCodes.get(detail) ?? "save_failed";
    error = { code, detail };
  }
  const after = [model.getLocale(), model.getTimezone(), model.getLanguage()];
  return {
    error,
    changed: before.some((value, index) => value !== after[index]),
  };
}

/** Redraw every applied change, including a partially successful save. */
export function saveAndRedrawRegionalSettings(
  model: RegionalModel,
  locale: string,
  timezone: string,
  language: string,
  redraw: () => void,
): RegionalSettingsSaveResult {
  const result = saveRegionalSettings(model, locale, timezone, language);
  if (result.changed) {
    redraw();
  }
  return result;
}
