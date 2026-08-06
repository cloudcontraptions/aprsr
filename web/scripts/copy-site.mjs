/**
 * Assemble the landing page into www/dist for GitHub Pages.
 *
 * Tailwind has already written site.css there; this copies everything else the deployed
 * site needs, including CNAME — a custom domain is lost the moment that file goes missing
 * from the published artifact.
 */

import { cp, mkdir, stat } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const www = resolve(here, "..", "..", "www");
const dist = join(www, "dist");

/** Files and directories copied verbatim. Missing optional entries are skipped. */
const ENTRIES = [
  { from: "index.html", required: true },
  { from: "CNAME", required: true },
  { from: "assets", required: false },
];

async function exists(path) {
  try {
    await stat(path);
    return true;
  } catch {
    return false;
  }
}

await mkdir(dist, { recursive: true });

for (const entry of ENTRIES) {
  const source = join(www, entry.from);
  if (!(await exists(source))) {
    if (entry.required) {
      console.error(`error: ${source} is missing`);
      process.exitCode = 1;
    }
    continue;
  }
  await cp(source, join(dist, entry.from), { recursive: true });
  console.log(`copied ${entry.from}`);
}
