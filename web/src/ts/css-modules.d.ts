/**
 * Stylesheets imported for their side effect.
 *
 * `map-entry.ts` imports Leaflet's stylesheet so esbuild bundles it and emits `map.css`
 * beside `map.js`. TypeScript has no idea what a `.css` file is and rejects the import
 * without this; the declaration says only "this is a module", which is all that is true —
 * the import has no value and nothing should try to use one.
 */
declare module "*.css";
