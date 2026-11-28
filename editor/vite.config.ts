// Builds the editor into crates/engine-admin/editor-dist/: one ES module
// (editor.mjs), one stylesheet (editor.css) and the licences of every
// third-party package in the bundle (editor-licenses.txt). File names
// carry no hash: the engine serves them under a name with the crate
// version. Run through scripts/build-editor.sh, never by hand.
import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";

/** Collect the licence of every npm package whose code ends up in the bundle. */
function licenses(): Plugin {
  return {
    name: "hammerwright-licenses",
    generateBundle(_opts, bundle) {
      const pkgs = new Map<string, string>();
      for (const chunk of Object.values(bundle)) {
        if (chunk.type !== "chunk") continue;
        for (const id of Object.keys(chunk.modules)) {
          const m = id.match(/node_modules\/((?:@[^/]+\/)?[^/]+)\//);
          if (m) pkgs.set(m[1], join(id.slice(0, id.indexOf(m[0]) + m[0].length)));
        }
      }
      let out =
        "Third-party code in Hammerwright's visual editor bundle (editor.mjs), with its licences.\n";
      for (const name of [...pkgs.keys()].sort()) {
        const dir = pkgs.get(name)!;
        const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
        out += `\n${"=".repeat(72)}\n${name} ${pkg.version} (${pkg.license})\n${"=".repeat(72)}\n`;
        const file = ["LICENSE", "LICENSE.md", "LICENSE.txt", "license", "LICENCE"]
          .map((f) => join(dir, f))
          .find((f) => existsSync(f));
        out += file ? readFileSync(file, "utf8").trim() + "\n" : `License: ${pkg.license}\n`;
      }
      this.emitFile({ type: "asset", fileName: "editor-licenses.txt", source: out });
    },
  };
}

export default defineConfig({
  plugins: [svelte(), licenses()],
  build: {
    outDir: "../crates/engine-admin/editor-dist",
    emptyOutDir: true,
    target: "es2022",
    minify: true,
    sourcemap: false,
    cssCodeSplit: false,
    reportCompressedSize: false,
    // One module for an admin tool, cached as immutable: ~530 kB, ~170 kB gzipped.
    chunkSizeWarningLimit: 700,
    modulePreload: false,
    rollupOptions: {
      input: "src/main.ts",
      output: {
        format: "es",
        entryFileNames: "editor.mjs",
        assetFileNames: "editor[extname]",
        codeSplitting: false,
      },
    },
  },
});
