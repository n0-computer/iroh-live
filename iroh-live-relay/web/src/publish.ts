import MoqPublish from "@moq/publish/element";

export { MoqPublish };

const publish = document.getElementById("publish") as MoqPublish | null;
if (!publish) throw new Error("missing <moq-publish> element");

const params = new URLSearchParams(window.location.search);
// A browser without a token may publish only under `anon/`.
const name = params.get("name") ?? "anon/hello";
const url = params.get("url") ?? `${window.location.origin}/`;

publish.setAttribute("url", url);
publish.setAttribute("name", name);
