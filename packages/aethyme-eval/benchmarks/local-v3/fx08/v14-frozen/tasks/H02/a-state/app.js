const options = [...document.querySelectorAll("#plan-options button")];
const summary = document.querySelector("#order-summary");

function updateSummaryFromProducer() {
  const producer = document.querySelector("[data-selected-plan]");
  const plan = producer?.getAttribute("data-selected-plan");
  const selected = options.find((option) => option.id === `plan-${plan}`);
  summary.textContent = selected?.id === "plan-pro"
    ? "Pro plan — $20/month"
    : "Basic plan — $10/month";
}

for (const option of options) {
  option.addEventListener("click", () => {
    for (const candidate of options) {
      candidate.setAttribute("aria-pressed", String(candidate === option));
      candidate.removeAttribute("data-selected-plan");
    }
    option.setAttribute("data-selected-plan", option.id.replace("plan-", ""));
    updateSummaryFromProducer();
  });
}
updateSummaryFromProducer();
