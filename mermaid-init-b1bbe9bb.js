// Draws the ```mermaid blocks of a page with mermaid.min.js, which the docs workflow places at
// the book's root (a pinned release, checked by its sha256; no CDN). It is loaded only on pages
// that have a diagram. Without the file, a block stays as its source text.
(() => {
  const blocks = document.querySelectorAll("code.language-mermaid");
  if (blocks.length === 0) return;
  const script = document.createElement("script");
  script.src = path_to_root + "mermaid.min.js";
  script.onload = () => {
    const html = document.documentElement.classList;
    const dark = ["coal", "navy", "ayu"].some((t) => html.contains(t));
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: dark ? "dark" : "neutral",
    });
    const nodes = [];
    for (const code of blocks) {
      const div = document.createElement("div");
      div.className = "mermaid";
      div.textContent = code.textContent;
      code.parentElement.replaceWith(div);
      nodes.push(div);
    }
    mermaid.run({ nodes });
  };
  document.head.appendChild(script);
})();
