export function bind(doc: Document): void {
  const start = doc.getElementById("start");
  start?.addEventListener("click", () => {
    const status = doc.getElementById("upload-status");
    if (status) status.textContent = "Uploading.";
  });
}
