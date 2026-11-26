// Every article: the Markdown files written by bench/run.py.
const mods = import.meta.glob("./content/*.md", { eager: true });
export const articles = Object.entries(mods)
  .map(([path, m]) => ({ slug: path.split("/").pop().replace(/\.md$/, ""), ...m }))
  .sort((a, b) => (a.frontmatter.date < b.frontmatter.date ? 1 : -1));
