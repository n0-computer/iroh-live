import MoqWatch from "@moq/watch/element";

export { MoqWatch };

const landing = document.getElementById("landing")!;
const player = document.getElementById("player")!;
const form = document.getElementById("connect-form") as HTMLFormElement;
const nameInput = document.getElementById("name-input") as HTMLInputElement;
const nowWatching = document.getElementById("now-watching")!;
const btnBack = document.getElementById("btn-back")!;
const watch = document.getElementById("watch") as MoqWatch;

const params = new URLSearchParams(window.location.search);
const baseUrl = params.get("url") ?? `${window.location.origin}/`;

function startWatch(name: string) {
  const trimmed = name.trim();
  if (!trimmed) return;

  console.log("watch", { url: baseUrl, name: trimmed });
  watch.url = baseUrl + trimmed;
  watch.name = "";

  nowWatching.textContent = trimmed;
  landing.style.display = "none";
  player.classList.add("active");

  // Update URL bar so the link is shareable.
  const u = new URL(window.location.href);
  u.searchParams.set("name", trimmed);
  history.replaceState(null, "", u.toString());
}

function goBack() {
  watch.url = "";
  player.classList.remove("active");
  landing.style.display = "";
  nameInput.focus();

  const u = new URL(window.location.href);
  u.searchParams.delete("name");
  history.replaceState(null, "", u.toString());
}

// If ?name= is already in the URL, jump straight to the player.
const initialName = params.get("name");
if (initialName) {
  nameInput.value = initialName;
  startWatch(initialName);
}

form.addEventListener("submit", (e) => {
  e.preventDefault();
  startWatch(nameInput.value);
});

btnBack.addEventListener("click", goBack);
