/* Hammerwright video_embed component: <hammerwright-video>, a third-party video that
   loads only when the visitor asks for it. The server HTML is a link to the
   video (it works without JavaScript); this adds a Play button that puts
   the provider's player in its place. No dependencies, no inline script. */
const ALLOW =
  "accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture; web-share";

class HammerwrightVideo extends HTMLElement {
  connectedCallback() {
    if (this.dataset.ready) return;
    const src = this.dataset.src || "";
    if (!/^https:\/\//.test(src)) return;
    this.dataset.ready = "1";
    this.style.display = "block";
    const title = this.dataset.title || "Video";
    const provider = this.dataset.provider || "the video's site";
    const play = document.createElement("button");
    play.type = "button";
    play.textContent = `Play “${title}”`;
    const note = document.createElement("small");
    note.textContent = ` Loads the player from ${provider}.`;
    const row = document.createElement("p");
    row.append(play, note);
    const link = this.querySelector(":scope > p");
    if (link) link.before(row);
    else this.append(row);
    play.addEventListener("click", () => {
      const url = new URL(src);
      url.searchParams.set("autoplay", "1");
      const frame = document.createElement("iframe");
      frame.src = url.href;
      frame.title = title;
      frame.allow = ALLOW;
      frame.allowFullscreen = true;
      frame.referrerPolicy = "strict-origin-when-cross-origin";
      this.replaceChildren(frame, ...(link ? [link] : []));
      frame.focus();
    });
  }
}

if (!customElements.get("hammerwright-video")) {
  customElements.define("hammerwright-video", HammerwrightVideo);
}
