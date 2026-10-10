export function wire(doc: Document): void {
  const button = doc.getElementById("send-signup");
  const status = doc.getElementById("status");
  button?.addEventListener("click", () => {
    if (status) status.textContent = "Sent.";
  });
  const email = doc.getElementById("email");
  email?.addEventListener("keydown", (event) => {
    if (event.key === "Enter") doc.getElementById("send-signup")?.click();
  });
}
