const entries = [
  { id: "fern", name: "Fern", category: "plants" },
  { id: "moss", name: "Moss", category: "plants" },
  { id: "amber", name: "Amber", category: "minerals" },
  { id: "slate", name: "Blue Slate", category: "minerals" }
];
const list = document.querySelector("#catalog-results");
let visibleEntries = [...entries];
let activeId = "fern";

function renderResults() {
  if (!visibleEntries.some((entry) => entry.id === activeId)) {
    activeId = visibleEntries[0]?.id ?? "";
  }
  list.setAttribute("aria-activedescendant", activeId ? `result-${activeId}` : "");
  list.replaceChildren(...visibleEntries.map((entry) => {
    const option = document.createElement("li");
    option.id = `result-${entry.id}`;
    option.setAttribute("role", "option");
    option.setAttribute("aria-selected", String(entry.id === activeId));
    option.textContent = entry.name;
    return option;
  }));
}

function moveActive(offset) {
  if (visibleEntries.length === 0) return;
  const current = visibleEntries.findIndex((entry) => entry.id === activeId);
  const next = (current + offset + visibleEntries.length) % visibleEntries.length;
  activeId = visibleEntries[next].id;
  renderResults();
}

function selectActive() {
  const entry = visibleEntries.find((candidate) => candidate.id === activeId);
  document.querySelector("#selection").textContent = entry
    ? `Selected: ${entry.name}`
    : "No item selected";
}

list.addEventListener("keydown", (event) => {
  if (event.key === "ArrowDown") {
    event.preventDefault();
    moveActive(1);
  } else if (event.key === "ArrowUp") {
    event.preventDefault();
    moveActive(-1);
  } else if (event.key === "Enter") {
    event.preventDefault();
    selectActive();
  }
});
document.querySelector("#select-active").addEventListener("click", selectActive);
renderResults();
