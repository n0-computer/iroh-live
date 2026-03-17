import MoqWatch from "@moq/watch/element";

export { MoqWatch };

const watch = document.getElementById("watch") as MoqWatch | null;
if (!watch) throw new Error("missing <moq-watch> element");

const params = new URLSearchParams(window.location.search);
const name = params.get("name") ?? "hello";
const url = params.get("url") ?? `${window.location.origin}/`;

watch.url = url;
watch.name = name;
