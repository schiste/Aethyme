import { mountPrimarySearch } from "./primary-search.js";

const catalog = ["Fern", "Moss", "Copper Fern", "Amber"];
mountPrimarySearch(document.querySelector("#primary-search-mount"));

let recentQuery = "";
const quickInput = document.querySelector("#quick-query");
const quickResults = document.querySelector("#quick-results");
document.querySelector("#quick-search-button").addEventListener("click", () => {
  recentQuery = quickInput.value.trim();
  const query = recentQuery.toLowerCase();
  const matches = catalog.filter((name) => name.toLowerCase().includes(query));
  quickResults.replaceChildren(...matches.map((name) => {
    const item = document.createElement("li");
    item.textContent = name;
    return item;
  }));
});
