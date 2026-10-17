const catalog = ["Fern", "Moss", "Copper Fern", "Amber"];

export function mountPrimarySearch(mount) {
  mount.innerHTML = `
    <section id="primary-search" class="panel">
      <h2>Main catalog search</h2>
      <div class="search-row">
        <label for="primary-query">Search catalog</label>
        <input id="primary-query" type="search">
        <button id="primary-search-button" type="button">Search primary</button>
      </div>
      <ul id="primary-results" class="result-list" aria-live="polite"></ul>
    </section>
  `;
  const input = mount.querySelector("#primary-query");
  const results = mount.querySelector("#primary-results");
  mount.querySelector("#primary-search-button").addEventListener("click", () => {
    const query = input.value.trim().toLowerCase();
    const matches = catalog.filter((name) => name.toLowerCase().includes(query));
    results.replaceChildren(...matches.map((name) => {
      const item = document.createElement("li");
      item.textContent = name;
      return item;
    }));
  });
}
