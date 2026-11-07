import type { Model } from "@ironcalc/wasm";
import { getAllTimezones, getSupportedLocales } from "@ironcalc/wasm";

export type RegionalSettingsSaveResult = { error: string | null };

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
  try {
    const localeChanged = locale !== model.getLocale();
    const timezoneChanged = timezone !== model.getTimezone();
    if (localeChanged && !getSupportedLocales().includes(locale)) {
      return { error: `Invalid locale: ${locale}` };
    }
    if (timezoneChanged && !getAllTimezones().includes(timezone)) {
      return { error: `Invalid timezone: ${timezone}` };
    }
    // Both context setters have the same paused/invalid-settings preconditions.
    // The first changed setter rejects before mutation when either applies.
    if (localeChanged) {
      model.setLocale(locale);
    }
    if (timezoneChanged) {
      model.setTimezone(timezone);
    }
    if (language !== model.getLanguage()) {
      model.setLanguage(language);
    }
    return { error: null };
  } catch (error) {
    return { error: error instanceof Error ? error.message : String(error) };
  }
}
