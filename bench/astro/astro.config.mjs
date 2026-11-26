// Static output, like `engine build`: one HTML file per page in dist/.
import { defineConfig } from "astro/config";
export default defineConfig({ output: "static", trailingSlash: "always", build: { format: "directory" } });
