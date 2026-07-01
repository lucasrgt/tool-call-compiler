// Marks the CJS build directory so Node treats its .js files as CommonJS.
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const cjsDir = path.resolve(here, "..", "dist", "cjs");
mkdirSync(cjsDir, { recursive: true });
writeFileSync(
  path.join(cjsDir, "package.json"),
  `${JSON.stringify({ type: "commonjs" }, null, 2)}\n`,
);
