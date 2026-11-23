import MoqPublish from "@moq/publish/element";

import { relayUrl, takeToken } from "./token";

export { MoqPublish };

const publish = document.getElementById("publish") as MoqPublish | null;
if (!publish) throw new Error("missing <moq-publish> element");

const params = new URLSearchParams(window.location.search);
const name = params.get("name") ?? "hello";
const base = params.get("url") ?? `${window.location.origin}/`;
const relay = relayUrl(base, "", takeToken());
if (relay.withheld) console.warn("not sending the token to a relay on another origin");

publish.setAttribute("url", relay.url);
publish.setAttribute("name", name);
