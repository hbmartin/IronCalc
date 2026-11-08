import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import svgr from "vite-plugin-svgr";
import pkg from "./package.json" with { type: "json" };

const EXTERNALS = ["@ironcalc/wasm", ...Object.keys(pkg.peerDependencies)];
// Share the engine generator's supported IDs; keep formula translations out of
// the browser bundle. Vite supplies the same constant to development and tests.
const formulaLanguageIds = Object.keys(
  JSON.parse(
    readFileSync(
      new URL("../../generate_language/src/languages.json", import.meta.url),
      "utf8",
    ),
  ),
);

function isExternal(id: string): boolean {
  return EXTERNALS.some(
    (externalPkg) => id === externalPkg || id.startsWith(`${externalPkg}/`),
  );
}

// https://vitejs.dev/config/
export default defineConfig({
  define: {
    __IC_FORMULA_LANGUAGE_IDS__: JSON.stringify(formulaLanguageIds),
  },
  build: {
    lib: {
      entry: resolve(import.meta.dirname, "src/index.ts"),
      name: "IronCalc",
      // the proper extensions will be added
      fileName: "ironcalc",
      formats: ["es"],
    },
    rolldownOptions: {
      external: isExternal,
    },
  },
  plugins: [react(), svgr()],
  server: {
    fs: {
      // Allow serving files from one level up to the project root
      allow: [".."],
    },
  },
});
