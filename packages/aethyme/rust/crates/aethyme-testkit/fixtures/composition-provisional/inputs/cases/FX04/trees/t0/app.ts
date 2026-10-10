export function wire(doc: Document): void {
  const button = doc.getElementById("submit");
  const status = doc.getElementById("status");
  button?.addEventListener("click", () => {
    if (status) status.textContent = "Sent.";
  });
}
