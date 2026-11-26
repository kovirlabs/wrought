import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  preprocess: vitePreprocess(),
  compilerOptions: {
    // No inline <style> injection at run time: component styles go to
    // editor.css (the admin's CSP allows only same-origin stylesheets for
    // scripts; inline styles are allowed, but a file is cleaner).
    css: "external",
  },
};
