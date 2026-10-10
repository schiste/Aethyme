import "./keyboard-shortcuts.js";

const entries = ["Fern", "Moss", "Copper Fern"];
const input = document.querySelector("#global-search");
const results = document.querySelector("#catalog-results");

function searchCatalog() {
  const query = input.value.trim().toLowerCase();
  const matches = entries.filter((name) => name.toLowerCase().includes(query));
  results.replaceChildren(...matches.map((name) => {
    const item = document.createElement("li");
    item.textContent = name;
    return item;
  }));
}

document.querySelector("#run-search").addEventListener("click", searchCatalog);
