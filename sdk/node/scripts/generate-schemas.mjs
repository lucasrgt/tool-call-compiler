// Generates src/schemas.gen.ts from the repository's /schemas JSON files —
// the single source of truth. Run automatically by the build/test scripts.
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const schemasDir = path.resolve(here, "..", "..", "..", "schemas");
const outFile = path.resolve(here, "..", "src", "schemas.gen.ts");

const load = (name) =>
    JSON.stringify(
        JSON.parse(readFileSync(path.join(schemasDir, name), "utf8")),
        null,
        2,
    );

const banner = `// GENERATED FILE — do not edit. Source of truth: /schemas/*.json.
// Regenerate with: npm run generate (runs automatically on build/test).
`;

const body = `${banner}
export const PLAN_SCHEMA = ${load("plan.schema.json")} as const;

export const INTENT_SCHEMA = ${load("intent.schema.json")} as const;

export const RECIPE_SCHEMA = ${load("recipe.schema.json")} as const;
`;

writeFileSync(outFile, body);
console.log(`generated ${path.relative(process.cwd(), outFile)}`);
