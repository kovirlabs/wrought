// Wrought's visual editor: mounts on #wrought-editor (admin/editor.html).
import { mount } from "svelte";
import App from "./App.svelte";
import { Editor } from "./state.svelte";
import "./editor.css";

const root = document.getElementById("wrought-editor");
if (root) {
  const d = root.dataset;
  const ed = new Editor(d.type ?? "", Number(d.id), d.csrf ?? "", d.editUrl ?? "", d.frameCss ?? "");
  root.replaceChildren();
  mount(App, { target: root, props: { ed } });
}
