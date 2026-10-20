const entries = [
  { name: "Fern", category: "plants" },
  { name: "Moss", category: "plants" },
  { name: "Amber", category: "minerals" },
  { name: "Blue Slate", category: "minerals" }
];
const boxes = [...document.querySelectorAll('input[name="category"]')];
const results = document.querySelector("#category-results");

document.querySelector("#apply-categories").addEventListener("click", () => {
  const selected = boxes.filter((box) => box.checked).map((box) => box.value);
  const matches = selected.length
    ? entries.filter((entry) => selected.includes(entry.category))
    : entries;
  results.replaceChildren(...matches.map((entry) => {
    const item = document.createElement("li");
    item.textContent = entry.name;
    return item;
  }));
});
